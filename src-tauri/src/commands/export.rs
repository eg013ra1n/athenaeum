//! Export-related Tauri commands
//!
//! Commands for exporting frame sets to external processing tools like Siril.
//!
//! Phase 5: Added new commands for v2 export workflow with ExportGroups.

use crate::commands::AppState;
use crate::export::{
    collect_export_data, collect_export_data_v3, organize_files_v4,
    organize_registered_files, count_registered_files,
    models::{
        CalibrationRoute, CalibrationRouteGroup, CalibrationRouteSummary, CalibrationTreeNode,
        DrizzleScale, ExportConfig, ExportData, ExportDataV3, ExportMode, ExportProgress,
        ExportResult, ExportStage, ExportTarget, ExptimeToleranceMode, GlobalRegistrationPlan,
        ImageWeightingMethod, ReferenceFrameMode, RejectionAlgorithm, SirilScriptPreview,
        SirilWorkflow,
    },
    siril::{find_siril_cli, generate_scripts_v3, run_siril_script},
};
use std::path::PathBuf;
use tauri::{Emitter, State};

/// Get export preview data for a frame set
///
/// Collects all light frames and their calibrations for preview before export.
#[tauri::command]
pub async fn get_export_preview(
    state: State<'_, AppState>,
    frame_set_id: i64,
) -> Result<ExportData, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    collect_export_data(&conn, frame_set_id).map_err(|e| e.to_string())
}

/// Get the configured or auto-detected Siril CLI path
#[tauri::command]
pub async fn get_siril_path(state: State<'_, AppState>) -> Result<Option<String>, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Try settings first
    let from_settings = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'siril_cli_path'",
            [],
            |row| row.get::<_, String>(0),
        )
        .ok();

    if from_settings.is_some() {
        return Ok(from_settings);
    }

    // Auto-detect
    Ok(find_siril_cli())
}

/// Set the Siril CLI path in settings
#[tauri::command]
pub async fn set_siril_path(state: State<'_, AppState>, path: String) -> Result<(), String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value, updated_at) VALUES ('siril_cli_path', ?1, datetime('now'))",
        [&path],
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}

/// Get a list of available frame sets for export
#[tauri::command]
pub async fn get_exportable_frame_sets(
    state: State<'_, AppState>,
) -> Result<Vec<ExportableFrameSet>, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
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
        .map_err(|e| e.to_string())?;

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
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    Ok(frame_sets)
}

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

/// Diagnostic data for debugging calibration links
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationDiagnostics {
    pub frame_set_id: i64,
    pub frame_ids_in_session_members: Vec<i64>,
    pub total_calibration_links_in_db: i64,
    pub links_for_frame_set_frames: Vec<CalibrationLinkInfo>,
    pub sample_source_ids_in_db: Vec<i64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationLinkInfo {
    pub source_id: i64,
    pub calibration_type: String,
    pub calibration_set_id: i64,
}

/// Diagnostic command to verify calibration links for a frame set
///
/// This helps debug why export shows zero calibrations even when links exist.
#[tauri::command]
pub async fn diagnose_calibration_links(
    state: State<'_, AppState>,
    frame_set_id: i64,
) -> Result<CalibrationDiagnostics, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get frame IDs from session_members (same query as data_collector)
    let mut id_stmt = conn.prepare(
        "SELECT DISTINCT sm.frame_id
         FROM session_members sm
         JOIN sessions s ON sm.session_id = s.id
         JOIN imaging_nights n ON s.imaging_night_id = n.id
         JOIN frames f ON sm.frame_id = f.id
         WHERE n.frames_set_id = ?1 AND f.imagetyp = 'Light'"
    ).map_err(|e| e.to_string())?;

    let frame_ids: Vec<i64> = id_stmt
        .query_map([frame_set_id], |row| row.get(0))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    println!("🔍 Diagnostics for frame set {}", frame_set_id);
    println!("  Frame IDs from session_members: {:?}", frame_ids);

    // Get total calibration links in DB
    let total_links: i64 = conn.query_row(
        "SELECT COUNT(*) FROM calibration_set_to_frames WHERE source_type = 'frame'",
        [],
        |row| row.get(0),
    ).map_err(|e| e.to_string())?;

    println!("  Total calibration links in DB: {}", total_links);

    // Get sample source_ids from DB
    let mut sample_stmt = conn.prepare(
        "SELECT DISTINCT source_id FROM calibration_set_to_frames WHERE source_type = 'frame' ORDER BY source_id LIMIT 20"
    ).map_err(|e| e.to_string())?;

    let sample_source_ids: Vec<i64> = sample_stmt
        .query_map([], |row| row.get(0))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    println!("  Sample source_ids in DB: {:?}", sample_source_ids);

    // Get links for each frame ID in the frame set
    let mut links = Vec::new();
    for frame_id in &frame_ids {
        let mut link_stmt = conn.prepare(
            "SELECT source_id, calibration_type, calibration_set_id
             FROM calibration_set_to_frames
             WHERE source_id = ?1 AND source_type = 'frame'"
        ).map_err(|e| e.to_string())?;

        let frame_links: Vec<CalibrationLinkInfo> = link_stmt
            .query_map([frame_id], |row| {
                Ok(CalibrationLinkInfo {
                    source_id: row.get(0)?,
                    calibration_type: row.get(1)?,
                    calibration_set_id: row.get(2)?,
                })
            })
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();

        println!("  Frame {}: {} links", frame_id, frame_links.len());
        links.extend(frame_links);
    }

    // Check if any frame IDs from session_members exist in sample_source_ids
    let matching_ids: Vec<i64> = frame_ids.iter()
        .filter(|id| sample_source_ids.contains(id))
        .cloned()
        .collect();
    println!("  Frame IDs that match source_ids in DB: {:?}", matching_ids);

    Ok(CalibrationDiagnostics {
        frame_set_id,
        frame_ids_in_session_members: frame_ids,
        total_calibration_links_in_db: total_links,
        links_for_frame_set_frames: links,
        sample_source_ids_in_db: sample_source_ids,
    })
}

// ============================================================================
// Phase 5: New Export Commands using ExportGroup Structure
// ============================================================================

/// Get calibration route for UI display
///
/// Returns a structured view of the export groups and their calibration trees,
/// suitable for displaying in the UI.
#[tauri::command]
pub async fn get_calibration_route(
    state: State<'_, AppState>,
    frame_set_id: i64,
) -> Result<CalibrationRoute, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Collect export data
    let export_data = collect_export_data(&conn, frame_set_id).map_err(|e| e.to_string())?;

    // Build calibration route from export data
    let mut groups = Vec::new();
    let mut total_lights = 0;
    let mut total_exposure = 0.0;
    let mut unique_calibration_sets = std::collections::HashSet::new();
    let mut all_warnings = Vec::new();

    for group in &export_data.groups {
        let mut calibration_tree = Vec::new();

        // Build calibration tree for this group
        // Start with Light node
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
                        label: format!("DarkFlat Set {} ({} frames)", dark_flat.set_id, dark_flat.frame_count),
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
                            label: format!("Bias Set {} ({} frames)", bias.set_id, bias.frame_count),
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
                        label: format!("Dark Set {} ({} frames)", dark.set_id, dark.frame_count),
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
                        label: format!("Bias Set {} ({} frames)", bias.set_id, bias.frame_count),
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
                    label: format!("Flat Set {} ({} frames)", flat.set_id, flat.frame_count),
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
                        label: format!("Bias Set {} ({} frames)", bias.set_id, bias.frame_count),
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
                    label: format!("Dark Set {} ({} frames)", dark.set_id, dark.frame_count),
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
                    label: format!("Bias Set {} ({} frames)", bias.set_id, bias.frame_count),
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

        // Add missing calibration warnings
        let has_flat = group.subgroups.first().and_then(|s| s.flat.as_ref()).is_some();
        let has_dark = group.subgroups.first().and_then(|s| s.dark.as_ref()).is_some();

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

    // Build script previews (simplified)
    let mut script_preview = Vec::new();

    if !export_data.master_plan.masters.is_empty() {
        script_preview.push(SirilScriptPreview {
            name: "00_create_masters.ssf".to_string(),
            description: format!(
                "Creates {} master calibration frames in dependency order",
                export_data.master_plan.masters.len()
            ),
            content: "# Master creation script preview\n# Run this first to create all calibration masters".to_string(),
        });
    }

    for group in &export_data.groups {
        let safe_name = group.group_key.replace(['/', '\\', ' '], "_");
        script_preview.push(SirilScriptPreview {
            name: format!("{}_preprocessing.ssf", safe_name),
            description: format!(
                "Preprocesses {} {} frames ({:.1}s total)",
                group.total_frames,
                group.display_name,
                group.total_exposure
            ),
            content: format!(
                "# Preprocessing script for {}\n# Camera type: {}",
                group.display_name,
                group.camera_type.display_name()
            ),
        });
    }

    // Build summary
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

    Ok(CalibrationRoute {
        groups,
        script_preview,
        summary,
    })
}

/// Get export groups summary for quick preview
///
/// Returns a lightweight summary of export groups without full calibration details.
#[tauri::command]
pub async fn get_export_groups_summary(
    state: State<'_, AppState>,
    frame_set_id: i64,
) -> Result<ExportGroupsSummary, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    let export_data = collect_export_data(&conn, frame_set_id).map_err(|e| e.to_string())?;

    let groups: Vec<ExportGroupInfo> = export_data
        .groups
        .iter()
        .map(|g| ExportGroupInfo {
            group_key: g.group_key.clone(),
            filter: g.filter.clone(),
            camera_type: g.camera_type.display_name().to_string(),
            display_name: g.display_name.clone(),
            frame_count: g.total_frames,
            total_exposure: g.total_exposure,
            subgroup_count: g.subgroups.len() as i32,
            has_flat: g.subgroups.first().and_then(|s| s.flat.as_ref()).is_some(),
            has_dark: g.subgroups.first().and_then(|s| s.dark.as_ref()).is_some(),
            has_bias: g.subgroups.first().and_then(|s| s.bias.as_ref()).is_some(),
            warnings: g.warnings.clone(),
        })
        .collect();

    Ok(ExportGroupsSummary {
        frame_set_id,
        total_groups: groups.len() as i32,
        total_frames: export_data.total_light_frames,
        total_exposure: export_data.total_exposure_seconds,
        masters_to_create: export_data.master_plan.masters.len() as i32,
        groups,
    })
}

/// Summary of export groups
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportGroupsSummary {
    pub frame_set_id: i64,
    pub total_groups: i32,
    pub total_frames: i32,
    pub total_exposure: f64,
    pub masters_to_create: i32,
    pub groups: Vec<ExportGroupInfo>,
}

/// Info about a single export group
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportGroupInfo {
    pub group_key: String,
    pub filter: Option<String>,
    pub camera_type: String,
    pub display_name: String,
    pub frame_count: i32,
    pub total_exposure: f64,
    pub subgroup_count: i32,
    pub has_flat: bool,
    pub has_dark: bool,
    pub has_bias: bool,
    pub warnings: Vec<String>,
}

// ============================================================================
// Phase 6: V3 Export with Nested Folder Hierarchy
// ============================================================================

/// Export a frame set using the V3/V4 workflow
///
/// Supports two export targets:
/// - Siril: Flat folder structure with generated scripts
/// - PixInsight WBPP: Grouped structure (camera/flats/lights) for auto-detection
#[tauri::command]
pub async fn export_frame_set_v3(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
    frame_set_id: i64,
    output_dir: String,
    mode: String,
    target: Option<String>,
    create_masters: bool,
    rejection_low: f64,
    rejection_high: f64,
    use_symlinks: bool,
    // Advanced Siril options
    reference_frame_mode: Option<String>,
    rejection_algorithm: Option<String>,
    image_weighting: Option<String>,
    drizzle_enabled: Option<bool>,
    drizzle_scale: Option<String>,
    // Exposure time grouping
    exptime_tolerance_mode: Option<String>,
    exptime_tolerance_value: Option<f64>,
) -> Result<ExportResult, String> {
    // Parse mode
    let export_mode = match mode.as_str() {
        "generate_scripts" => ExportMode::GenerateScripts,
        "organize_files" => ExportMode::OrganizeFiles,
        "organize_and_script" => ExportMode::OrganizeAndScript,
        "direct_execution" => ExportMode::DirectExecution,
        _ => return Err(format!("Invalid export mode: {}", mode)),
    };

    // Parse target (default to Siril for backwards compatibility)
    let export_target = match target.as_deref() {
        Some("pixinsight_wbpp") | Some("wbpp") => ExportTarget::PixInsightWBPP,
        _ => ExportTarget::Siril,
    };

    // Parse advanced Siril options
    let ref_mode = match reference_frame_mode.as_deref() {
        Some("athenaeum_scoring") => ReferenceFrameMode::AtheneumScoring,
        Some("manual") => ReferenceFrameMode::Manual,
        _ => ReferenceFrameMode::SirilAuto,
    };

    let rej_algo = match rejection_algorithm.as_deref() {
        Some("percentile") => RejectionAlgorithm::Percentile,
        Some("linear_fit") => RejectionAlgorithm::LinearFit,
        Some("gesd") => RejectionAlgorithm::Gesd,
        Some("mad") => RejectionAlgorithm::Mad,
        _ => RejectionAlgorithm::Sigma,
    };

    let weighting = match image_weighting.as_deref() {
        Some("none") => ImageWeightingMethod::None,
        Some("stars") => ImageWeightingMethod::Stars,
        Some("noise") => ImageWeightingMethod::Noise,
        Some("exposure_time") => ImageWeightingMethod::ExposureTime,
        _ => ImageWeightingMethod::Wfwhm,
    };

    let drizzle_scl = match drizzle_scale.as_deref() {
        Some("x3") => DrizzleScale::X3,
        Some("x2") => DrizzleScale::X2,
        _ => DrizzleScale::None,
    };

    // Parse exposure time tolerance settings
    let exptime_mode = match exptime_tolerance_mode.as_deref() {
        Some("absolute") => ExptimeToleranceMode::Absolute,
        Some("relative") => ExptimeToleranceMode::Relative,
        _ => ExptimeToleranceMode::Disabled,
    };
    let exptime_value = exptime_tolerance_value.unwrap_or(30.0);

    // Build config
    let config = ExportConfig {
        frame_set_id,
        output_dir: PathBuf::from(&output_dir),
        target: export_target.clone(),
        mode: export_mode.clone(),
        workflow: SirilWorkflow::MonoPreprocessing, // Will be determined per branch
        create_masters,
        rejection_low,
        rejection_high,
        use_symlinks,
        // Advanced Siril options
        reference_frame_mode: ref_mode,
        manual_reference_frame_id: None,
        rejection_algorithm: rej_algo,
        image_weighting: weighting,
        drizzle_enabled: drizzle_enabled.unwrap_or(false),
        drizzle_scale: drizzle_scl,
        // Exposure time grouping
        exptime_tolerance_mode: exptime_mode,
        exptime_tolerance_value: exptime_value,
    };

    // Emit collecting progress
    let _ = app_handle.emit(
        "export-progress",
        ExportProgress {
            stage: ExportStage::Collecting,
            progress: 0.0,
            message: "Collecting frame data (V3)...".to_string(),
            current_file: None,
        },
    );

    // Collect export data using V3 collector
    let export_data = {
        let state_lock = state.db.lock().unwrap();
        let db = state_lock.as_ref().ok_or("Database not initialized")?;
        let conn = db.conn();
        collect_export_data_v3(&conn, frame_set_id).map_err(|e| e.to_string())?
    };

    // Emit data collection complete
    let _ = app_handle.emit(
        "export-progress",
        ExportProgress {
            stage: ExportStage::Collecting,
            progress: 100.0,
            message: format!(
                "Found {} light frames in {} branches across {} cameras",
                export_data.total_light_frames,
                export_data.branches.len(),
                export_data.cameras.len()
            ),
            current_file: None,
        },
    );

    let mut result = ExportResult {
        success: true,
        output_dir: output_dir.clone(),
        files_organized: 0,
        scripts_generated: Vec::new(),
        warnings: Vec::new(),
        error: None,
    };

    // Step 1: Organize files (if needed)
    if matches!(
        export_mode,
        ExportMode::OrganizeFiles | ExportMode::OrganizeAndScript | ExportMode::DirectExecution
    ) {
        let organize_message = match export_target {
            ExportTarget::Siril => "Organizing files for Siril...",
            ExportTarget::PixInsightWBPP => "Organizing files for PixInsight WBPP...",
        };
        let _ = app_handle.emit(
            "export-progress",
            ExportProgress {
                stage: ExportStage::Organizing,
                progress: 0.0,
                message: organize_message.to_string(),
                current_file: None,
            },
        );

        match organize_files_v4(&config, &export_data) {
            Ok(org_result) => {
                result.files_organized = org_result.files_organized;
                result.warnings.extend(org_result.warnings);

                let _ = app_handle.emit(
                    "export-progress",
                    ExportProgress {
                        stage: ExportStage::Organizing,
                        progress: 100.0,
                        message: format!("Organized {} files", org_result.files_organized),
                        current_file: None,
                    },
                );
            }
            Err(e) => {
                let _ = app_handle.emit(
                    "export-progress",
                    ExportProgress {
                        stage: ExportStage::Failed,
                        progress: 0.0,
                        message: format!("Failed to organize files: {}", e),
                        current_file: None,
                    },
                );
                return Ok(ExportResult {
                    success: false,
                    output_dir,
                    files_organized: 0,
                    scripts_generated: Vec::new(),
                    warnings: Vec::new(),
                    error: Some(format!("Failed to organize files: {}", e)),
                });
            }
        }
    }

    // Step 2: Generate scripts (Siril only)
    // WBPP doesn't need scripts - it auto-detects from FITS keywords
    if export_target == ExportTarget::Siril
        && matches!(
            export_mode,
            ExportMode::GenerateScripts | ExportMode::OrganizeAndScript | ExportMode::DirectExecution
        )
    {
        let _ = app_handle.emit(
            "export-progress",
            ExportProgress {
                stage: ExportStage::GeneratingScripts,
                progress: 0.0,
                message: "Generating Siril scripts...".to_string(),
                current_file: None,
            },
        );

        // TODO: Update to use new Siril folder structure
        // For now, use v3 generator (will need updating for new flat paths)
        match generate_scripts_v3(&config, &export_data) {
            Ok(scripts) => {
                result.scripts_generated = scripts
                    .iter()
                    .map(|p| p.to_string_lossy().to_string())
                    .collect();

                let _ = app_handle.emit(
                    "export-progress",
                    ExportProgress {
                        stage: ExportStage::GeneratingScripts,
                        progress: 100.0,
                        message: format!("Generated {} scripts", result.scripts_generated.len()),
                        current_file: None,
                    },
                );
            }
            Err(e) => {
                let _ = app_handle.emit(
                    "export-progress",
                    ExportProgress {
                        stage: ExportStage::Failed,
                        progress: 0.0,
                        message: format!("Failed to generate scripts: {}", e),
                        current_file: None,
                    },
                );
                return Ok(ExportResult {
                    success: false,
                    output_dir,
                    files_organized: result.files_organized,
                    scripts_generated: Vec::new(),
                    warnings: result.warnings,
                    error: Some(format!("Failed to generate scripts: {}", e)),
                });
            }
        }
    }

    // Step 3: Execute Siril (if direct execution mode and Siril target)
    if export_target == ExportTarget::Siril && export_mode == ExportMode::DirectExecution {
        use crate::export::siril::collect_calibrated_frames;

        let siril_path = {
            let state_lock = state.db.lock().unwrap();
            let db = state_lock.as_ref().ok_or("Database not initialized")?;
            let conn = db.conn();

            conn.query_row(
                "SELECT value FROM settings WHERE key = 'siril_cli_path'",
                [],
                |row| row.get::<_, String>(0),
            )
            .ok()
        };

        let siril_path = siril_path
            .or_else(find_siril_cli)
            .ok_or("Siril CLI not found. Please configure the path in settings.")?;

        let output_path = PathBuf::from(&output_dir);

        // Run scripts in order with frame collection between calibration and registration
        // Order: 00_create_masters → 01_calibrate_lights → [collect frames] → 02_register_and_stack
        for script_path in &result.scripts_generated {
            let script_name = PathBuf::from(script_path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();

            let _ = app_handle.emit(
                "export-progress",
                ExportProgress {
                    stage: ExportStage::SirilCalibrating,
                    progress: 0.0,
                    message: format!("Running {}...", script_name),
                    current_file: Some(script_path.clone()),
                },
            );

            if let Err(e) =
                run_siril_script(&siril_path, &PathBuf::from(script_path), &app_handle)
            {
                result
                    .warnings
                    .push(format!("Script {} failed: {}", script_path, e));
                // Don't continue if a script fails
                break;
            }

            // After calibration script, collect frames before registration
            if script_name.contains("01_calibrate") {
                let _ = app_handle.emit(
                    "export-progress",
                    ExportProgress {
                        stage: ExportStage::CollectingCalibratedFrames,
                        progress: 0.0,
                        message: "Collecting calibrated frames...".to_string(),
                        current_file: None,
                    },
                );

                match collect_calibrated_frames(&output_path, &app_handle) {
                    Ok(collected) => {
                        println!("✅ Collected {} calibrated frames ({} mono, {} OSC)",
                            collected.total(), collected.mono_count(), collected.osc_count());
                    }
                    Err(e) => {
                        result.warnings.push(format!("Failed to collect frames: {}", e));
                        // This is critical - can't continue without collected frames
                        break;
                    }
                }
            }
        }
    }

    // Emit export complete
    let _ = app_handle.emit(
        "export-progress",
        ExportProgress {
            stage: ExportStage::Complete,
            progress: 100.0,
            message: format!(
                "Export complete! {} files, {} scripts",
                result.files_organized,
                result.scripts_generated.len()
            ),
            current_file: None,
        },
    );

    Ok(result)
}

/// Get V3 export preview data for a frame set
///
/// Returns nested hierarchy structure suitable for preview before export.
#[tauri::command]
pub async fn get_export_preview_v3(
    state: State<'_, AppState>,
    frame_set_id: i64,
) -> Result<ExportDataV3, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    collect_export_data_v3(&conn, frame_set_id).map_err(|e| e.to_string())
}

// ============================================================================
// Post-Registration File Organization Commands
// ============================================================================

/// Result of organizing registered files
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrganizeRegisteredResult {
    pub success: bool,
    pub files_organized: usize,
    pub filters_created: Vec<String>,
    pub warnings: Vec<String>,
    pub error: Option<String>,
}

/// Organize globally registered files into filter-specific folders
///
/// This command is called AFTER script 02 (global registration) completes.
/// It organizes the registered files (r_all_lights_*.fit) into filter-specific
/// folders so that script 03 can stack them by filter.
///
/// # Arguments
/// * `frame_set_id` - ID of the frame set being exported
/// * `output_dir` - Root output directory of the export
/// * `use_symlinks` - Whether to use symlinks instead of copying files
#[tauri::command]
pub async fn organize_registered_files_cmd(
    state: State<'_, AppState>,
    frame_set_id: i64,
    output_dir: String,
    use_symlinks: bool,
) -> Result<OrganizeRegisteredResult, String> {
    // First, check if registered files exist
    let output_path = PathBuf::from(&output_dir);
    let registered_count = count_registered_files(&output_path);

    if registered_count == 0 {
        return Ok(OrganizeRegisteredResult {
            success: false,
            files_organized: 0,
            filters_created: Vec::new(),
            warnings: Vec::new(),
            error: Some("No registered files found. Run script 02_register_globally.ssf first.".to_string()),
        });
    }

    // Collect export data to get the GlobalRegistrationPlan
    let export_data = {
        let state_lock = state.db.lock().unwrap();
        let db = state_lock.as_ref().ok_or("Database not initialized")?;
        let conn = db.conn();
        collect_export_data_v3(&conn, frame_set_id).map_err(|e| e.to_string())?
    };

    // Create the registration plan
    let plan = GlobalRegistrationPlan::from_export_data(&export_data);

    // Verify frame counts match
    if plan.total_frames != registered_count {
        println!(
            "Warning: Expected {} registered files, found {}",
            plan.total_frames, registered_count
        );
    }

    // Organize the registered files
    match organize_registered_files(&output_path, &plan, use_symlinks) {
        Ok(result) => Ok(OrganizeRegisteredResult {
            success: true,
            files_organized: result.files_organized,
            filters_created: result.filters_created,
            warnings: result.warnings,
            error: None,
        }),
        Err(e) => Ok(OrganizeRegisteredResult {
            success: false,
            files_organized: 0,
            filters_created: Vec::new(),
            warnings: Vec::new(),
            error: Some(format!("Failed to organize registered files: {}", e)),
        }),
    }
}

/// Check the status of registered files after global registration
///
/// Returns information about registered files in the process folder.
#[tauri::command]
pub async fn get_registered_files_status(
    output_dir: String,
) -> Result<RegisteredFilesStatus, String> {
    let output_path = PathBuf::from(&output_dir);
    let process_dir = output_path.join("process");

    let total_registered = count_registered_files(&output_path);

    // Check if files have been organized
    let registered_dir = process_dir.join("registered");
    let organized = registered_dir.exists();

    let mut organized_by_filter: Vec<FilterOrganizedInfo> = Vec::new();
    if organized {
        if let Ok(entries) = std::fs::read_dir(&registered_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                    let filter_name = entry.file_name().to_string_lossy().to_string();
                    let filter_dir = entry.path();

                    let file_count = std::fs::read_dir(&filter_dir)
                        .map(|entries| {
                            entries
                                .filter_map(|e| e.ok())
                                .filter(|e| {
                                    e.path().extension()
                                        .map(|ext| ext == "fit")
                                        .unwrap_or(false)
                                })
                                .count()
                        })
                        .unwrap_or(0);

                    organized_by_filter.push(FilterOrganizedInfo {
                        filter_name,
                        file_count,
                    });
                }
            }
        }
    }

    Ok(RegisteredFilesStatus {
        total_registered,
        organized,
        organized_by_filter,
    })
}

/// Status of registered files
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisteredFilesStatus {
    pub total_registered: usize,
    pub organized: bool,
    pub organized_by_filter: Vec<FilterOrganizedInfo>,
}

/// Info about files organized for a specific filter
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterOrganizedInfo {
    pub filter_name: String,
    pub file_count: usize,
}

// ============================================================================
// Pipeline Execution Commands
// ============================================================================

/// Run the complete Siril export pipeline automatically
///
/// This command orchestrates all export steps in sequence:
/// 1. Create calibration masters (if 00_create_masters.ssf exists)
/// 2. Calibrate light frames (01_calibrate_lights.ssf)
/// 3. Collect calibrated frames to unified directory
/// 4. Register and stack (02_register_and_stack.ssf)
///
/// Progress events are emitted throughout the process.
#[tauri::command]
pub async fn run_siril_export_pipeline(
    app_handle: tauri::AppHandle,
    export_dir: String,
) -> Result<String, String> {
    use crate::export::siril::run_export_pipeline;

    let export_path = PathBuf::from(&export_dir);

    // Verify the directory exists
    if !export_path.exists() {
        return Err(format!("Export directory does not exist: {}", export_dir));
    }

    // Run the pipeline
    run_export_pipeline(&export_path, &app_handle)
        .map_err(|e| e.to_string())?;

    Ok("Export pipeline completed successfully".to_string())
}

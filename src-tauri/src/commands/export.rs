//! Export-related Tauri commands
//!
//! Commands for exporting frame sets to PixInsight WBPP folder structure.

use crate::commands::AppState;
use crate::export::{
    collect_export_data, collect_export_summary, organize_files_wbpp,
    models::{
        CalibrationRoute, CalibrationRouteGroup, CalibrationRouteSummary, CalibrationTreeNode,
        ExportData, ExportResult, ExportSummary,
    },
};
use std::path::PathBuf;
use tauri::State;

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
        summary,
    })
}

/// Export a frame set to PixInsight WBPP folder structure
///
/// Creates a folder structure optimized for PixInsight's Weighted Batch Preprocessing (WBPP):
/// ```
/// output/
/// └── camera_{name}/
///     ├── darks/
///     │   └── (bias, dark, darkflat files)
///     └── flats_{filter}/
///         └── lights/
///             └── (light frames)
/// ```
#[tauri::command]
pub async fn export_to_wbpp(
    state: State<'_, AppState>,
    frame_set_id: i64,
    output_dir: String,
    use_symlinks: bool,
) -> Result<ExportResult, String> {
    // Collect export data
    let export_data = {
        let state_lock = state.db.lock().unwrap();
        let db = state_lock.as_ref().ok_or("Database not initialized")?;
        let conn = db.conn();
        collect_export_data(&conn, frame_set_id).map_err(|e| e.to_string())?
    };

    let output_path = PathBuf::from(&output_dir);

    // Organize files into WBPP structure
    match organize_files_wbpp(&output_path, &export_data, use_symlinks) {
        Ok(org_result) => Ok(ExportResult {
            success: true,
            output_dir,
            files_organized: org_result.files_organized,
            scripts_generated: Vec::new(),
            warnings: org_result.warnings,
            error: None,
        }),
        Err(e) => Ok(ExportResult {
            success: false,
            output_dir,
            files_organized: 0,
            scripts_generated: Vec::new(),
            warnings: Vec::new(),
            error: Some(format!("Failed to organize files: {}", e)),
        }),
    }
}

/// Get enhanced export summary for the new UI
///
/// Returns comprehensive export data with:
/// - Equipment info (cameras, telescopes, date range)
/// - Filter groups with exposure breakdown
/// - Calibration details with match quality
/// - Folder structure preview
/// - Detailed warnings with context
#[tauri::command]
pub async fn get_export_summary(
    state: State<'_, AppState>,
    frame_set_id: i64,
) -> Result<ExportSummary, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    collect_export_summary(&conn, frame_set_id).map_err(|e| e.to_string())
}

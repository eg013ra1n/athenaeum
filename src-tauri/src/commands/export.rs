//! Export-related Tauri commands
//!
//! Commands for exporting frame sets to external processing tools like Siril.

use crate::commands::AppState;
use crate::export::{
    collect_export_data, organize_files,
    models::{ExportConfig, ExportData, ExportMode, ExportResult, SirilWorkflow},
    siril::{find_siril_cli, generate_scripts, run_siril_script},
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

/// Export a frame set with the given configuration
///
/// Depending on the mode, this will:
/// - GenerateScripts: Only generate Siril scripts
/// - OrganizeFiles: Only organize files into folders
/// - OrganizeAndScript: Organize files and generate scripts
/// - DirectExecution: Organize, generate scripts, and run Siril
#[tauri::command]
pub async fn export_frame_set(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
    frame_set_id: i64,
    output_dir: String,
    mode: String,
    workflow: String,
    rejection_low: f64,
    rejection_high: f64,
    use_symlinks: bool,
) -> Result<ExportResult, String> {
    // Parse mode
    let export_mode = match mode.as_str() {
        "generate_scripts" => ExportMode::GenerateScripts,
        "organize_files" => ExportMode::OrganizeFiles,
        "organize_and_script" => ExportMode::OrganizeAndScript,
        "direct_execution" => ExportMode::DirectExecution,
        _ => return Err(format!("Invalid export mode: {}", mode)),
    };

    // Parse workflow
    let siril_workflow = match workflow.as_str() {
        "mono_preprocessing" => SirilWorkflow::MonoPreprocessing,
        "osc_preprocessing" => SirilWorkflow::OscPreprocessing,
        "lrgb_processing" => SirilWorkflow::LrgbProcessing,
        _ => return Err(format!("Invalid workflow: {}", workflow)),
    };

    // Build config
    let config = ExportConfig {
        frame_set_id,
        output_dir: PathBuf::from(&output_dir),
        mode: export_mode.clone(),
        workflow: siril_workflow,
        create_masters: true,
        rejection_low,
        rejection_high,
        use_symlinks,
    };

    // Collect export data
    let export_data = {
        let state_lock = state.db.lock().unwrap();
        let db = state_lock.as_ref().ok_or("Database not initialized")?;
        let conn = db.conn();
        collect_export_data(&conn, frame_set_id).map_err(|e| e.to_string())?
    };

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
        match organize_files(&config, &export_data) {
            Ok(org_result) => {
                result.files_organized = org_result.files_organized;
                result.warnings.extend(org_result.warnings);
            }
            Err(e) => {
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

    // Step 2: Generate scripts (if needed)
    if matches!(
        export_mode,
        ExportMode::GenerateScripts | ExportMode::OrganizeAndScript | ExportMode::DirectExecution
    ) {
        match generate_scripts(&config, &export_data) {
            Ok(scripts) => {
                result.scripts_generated = scripts
                    .iter()
                    .map(|p| p.to_string_lossy().to_string())
                    .collect();
            }
            Err(e) => {
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

    // Step 3: Execute Siril (if direct execution mode)
    if export_mode == ExportMode::DirectExecution {
        // Get Siril path from settings or auto-detect
        let siril_path = {
            let state_lock = state.db.lock().unwrap();
            let db = state_lock.as_ref().ok_or("Database not initialized")?;
            let conn = db.conn();

            // Try to get from settings first
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

        // Run each script
        for script_path in &result.scripts_generated {
            if let Err(e) = run_siril_script(&siril_path, &PathBuf::from(script_path), &app_handle)
            {
                result.warnings.push(format!(
                    "Script {} failed: {}",
                    script_path,
                    e
                ));
            }
        }
    }

    Ok(result)
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

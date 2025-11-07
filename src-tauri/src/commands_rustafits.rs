// Rustafits-based commands for FITS image processing

use crate::cache::{StretchMode, StretchParams};
use crate::commands::AppState;
use crate::rustafits_processor::{self, Resolution};
use std::path::PathBuf;
use tauri::State;

/// Read FITS image and return as JPEG using rustafits
/// Uses rustafits for fast, high-quality FITS to JPEG conversion with auto-stretch
#[tauri::command]
pub async fn read_fits_image_rustafits(
    path: String,
    resolution: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<u8>, String> {
    use std::time::Instant;

    let t_start = Instant::now();
    let path_buf = PathBuf::from(&path);

    println!("\n⚡ === RUSTAFITS IMAGE REQUEST ===");
    println!("📁 Path: {}", path_buf.display());
    println!("🎯 Resolution: {:?}", resolution);

    // Get resolution from parameter or database setting
    let resolution_str = if let Some(res_param) = resolution.as_deref() {
        res_param.to_string()
    } else {
        // Read from database setting
        let state_lock = state.db.lock().unwrap();
        if let Some(db) = state_lock.as_ref() {
            let conn = db.conn();
            match crate::db::get_setting(&conn, "blink.resolution") {
                Ok(Some(value)) => value,
                _ => "preview".to_string(), // Default to preview
            }
        } else {
            "preview".to_string()
        }
    };

    // Parse resolution parameter
    let res = match resolution_str.as_str() {
        "thumbnail" => Resolution::Thumbnail,
        "preview" => Resolution::Preview,
        "full" => Resolution::Full,
        _ => Resolution::Preview,
    };

    // Check if file exists
    if !path_buf.exists() {
        let error_msg = format!("File not found: {}", path_buf.display());
        eprintln!("❌ ERROR: {}", error_msg);
        return Err(error_msg);
    }

    // Read quality setting from database
    let quality = {
        let state_lock = state.db.lock().unwrap();
        if let Some(db) = state_lock.as_ref() {
            let conn = db.conn();
            let quality_key = res.quality_setting_key();
            match crate::db::get_setting(&conn, quality_key) {
                Ok(Some(value)) => value.parse().ok(),
                _ => None,
            }
        } else {
            None
        }
    };

    // Try to use cache if available
    let cache_mgr_exists = state.cache.lock().unwrap().is_some();

    if cache_mgr_exists {
        println!("📦 Cache manager available, attempting cached read...");

        // Get file info from database or create minimal entry
        let file_info = {
            let state_lock = state.db.lock().unwrap();
            if let Some(db) = state_lock.as_ref() {
                let conn = db.conn();
                match crate::db::get_file_by_path(&conn, &path) {
                    Ok(file) => {
                        println!("✅ File found in database: id={:?}", file.id);
                        Some(file)
                    }
                    Err(e) => {
                        println!("⚠️  File not in database ({}), creating minimal entry", e);
                        // Create a minimal File object for cache to work
                        let metadata = std::fs::metadata(&path_buf).ok();
                        Some(crate::models::File {
                            id: Some(-1), // Use -1 to indicate non-database file
                            path: path.clone(),
                            filename: path_buf
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("unknown")
                                .to_string(),
                            size: metadata.as_ref().map(|m| m.len() as i64).unwrap_or(0),
                            modified_at: metadata
                                .as_ref()
                                .and_then(|m| m.modified().ok())
                                .map(|t| chrono::DateTime::<chrono::Utc>::from(t))
                                .unwrap_or_else(|| chrono::Utc::now()),
                            format: crate::models::FileFormat::FITS,
                            created_at: chrono::Utc::now(),
                            metadata_hash: None,
                        })
                    }
                }
            } else {
                println!("⚠️  Database not initialized");
                None
            }
        };

        if let Some(file) = file_info {
            // Create stretch params (rustafits uses auto-stretch internally)
            let stretch_params = StretchParams {
                mode: StretchMode::Auto,
                black_point: 0,
                white_point: 0,
                midtones: 0.35, // rustafits default
                resolution: resolution_str.clone(),
            };

            // Try to get from cache
            let cache_arc = state.cache.clone();
            let cached_data_result = {
                let cache_guard = cache_arc.lock().unwrap();
                if let Some(cache_mgr) = cache_guard.as_ref() {
                    use tokio::runtime::Handle;
                    use tokio::task;

                    task::block_in_place(|| {
                        Handle::current().block_on(async {
                            cache_mgr
                                .get_or_create(&file, &path_buf, &stretch_params, quality)
                                .await
                        })
                    })
                } else {
                    Err(anyhow::anyhow!("Cache manager not available"))
                }
            };

            match cached_data_result {
                Ok(cached_data) => {
                    println!("✅ Got data from cache ({} bytes)", cached_data.len());
                    println!("⏱️  Total time: {:?}", t_start.elapsed());
                    println!("=== END ===\n");
                    return Ok(cached_data);
                }
                Err(e) => {
                    eprintln!("Cache error: {}", e);
                    // Fall through to direct processing
                }
            }
        }
    }

    // Fallback: Direct processing without cache
    println!("⚠️  Processing without cache...");
    println!("🎨 Using quality: {:?}", quality);

    let result = rustafits_processor::process_fits_to_jpeg(&path_buf, res, quality)
        .map_err(|e| {
            let error_msg = format!("Failed to process FITS image: {}", e);
            eprintln!("❌ ERROR: {}", error_msg);
            eprintln!("   Path: {}", path_buf.display());
            eprintln!("   Error details: {:?}", e);
            error_msg
        })?;

    println!("✅ Processing complete in {:?}", t_start.elapsed());
    println!("📦 Returning {} bytes", result.image_data.len());
    println!("=== END ===\n");

    Ok(result.image_data)
}

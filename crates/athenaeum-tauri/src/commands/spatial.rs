// Spatial query commands - sky coordinates and location-based operations

use crate::models::*;
use tauri::State;

use super::utils::calculate_fov;
use super::AppState;

/// Get all imaging locations (both organized frame sets and unorganized clusters)
///
/// Returns a list of all locations where frames have been taken, including:
/// - Organized locations: Frames that are part of frame sets
/// - Unorganized locations: Frames not in any set, clustered by sky coordinates
#[tauri::command]
pub async fn get_imaging_locations(state: State<'_, AppState>) -> Result<Vec<ImagingLocation>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Query both organized frame sets AND unorganized frames
    // This enables users to see all frames with coordinates immediately,
    // without needing to auto-generate frame sets first
    let mut stmt = conn.prepare("
        -- Organized locations: Frames in frame sets
        SELECT
            fs.id as frame_set_id,
            fs.name as object_name,
            AVG(fr.ra) as avg_ra,
            AVG(fr.dec) as avg_dec,
            COUNT(DISTINCT fr.id) as frame_count,
            COALESCE(SUM(fr.exptime), 0) as total_exposure,
            GROUP_CONCAT(DISTINCT fr.filter) as filters,
            MIN(fr.date_obs) as first_date,
            MAX(fr.date_obs) as last_date,
            AVG(fr.xpixsz) as avg_xpixsz,
            AVG(fr.ypixsz) as avg_ypixsz,
            AVG(fr.focallen) as avg_focallen,
            AVG(fr.naxis1) as avg_naxis1,
            AVG(fr.naxis2) as avg_naxis2,
            AVG(fr.xbinning) as avg_xbinning,
            AVG(fr.ybinning) as avg_ybinning,
            'frameset' as location_type,
            GROUP_CONCAT(DISTINCT fr.instrume) as cameras,
            GROUP_CONCAT(DISTINCT CAST(fr.focallen AS TEXT)) as focal_lengths,
            fs.is_custom as is_custom,
            AVG(SIN(fr.rotation * 3.14159265358979 / 180.0)) as avg_sin_rot,
            AVG(COS(fr.rotation * 3.14159265358979 / 180.0)) as avg_cos_rot
        FROM frames_set fs
        JOIN imaging_nights ino ON ino.frames_set_id = fs.id
        JOIN sessions s ON s.imaging_night_id = ino.id
        JOIN session_members sm ON sm.session_id = s.id
        JOIN frames fr ON fr.id = sm.frame_id
        WHERE fr.ra IS NOT NULL
          AND fr.dec IS NOT NULL
          AND fr.imagetyp = 'Light'
        GROUP BY fs.id
        HAVING avg_ra IS NOT NULL AND avg_dec IS NOT NULL

        UNION ALL

        -- Unorganized locations: Frames NOT in any session, clustered by location
        SELECT
            NULL as frame_set_id,
            COALESCE(fr.object, 'Unknown') as object_name,
            AVG(fr.ra) as avg_ra,
            AVG(fr.dec) as avg_dec,
            COUNT(DISTINCT fr.id) as frame_count,
            COALESCE(SUM(fr.exptime), 0) as total_exposure,
            GROUP_CONCAT(DISTINCT fr.filter) as filters,
            MIN(fr.date_obs) as first_date,
            MAX(fr.date_obs) as last_date,
            AVG(fr.xpixsz) as avg_xpixsz,
            AVG(fr.ypixsz) as avg_ypixsz,
            AVG(fr.focallen) as avg_focallen,
            AVG(fr.naxis1) as avg_naxis1,
            AVG(fr.naxis2) as avg_naxis2,
            AVG(fr.xbinning) as avg_xbinning,
            AVG(fr.ybinning) as avg_ybinning,
            'cluster' as location_type,
            GROUP_CONCAT(DISTINCT fr.instrume) as cameras,
            GROUP_CONCAT(DISTINCT CAST(fr.focallen AS TEXT)) as focal_lengths,
            0 as is_custom,
            AVG(SIN(fr.rotation * 3.14159265358979 / 180.0)) as avg_sin_rot,
            AVG(COS(fr.rotation * 3.14159265358979 / 180.0)) as avg_cos_rot
        FROM frames fr
        WHERE fr.ra IS NOT NULL
          AND fr.dec IS NOT NULL
          AND fr.imagetyp = 'Light'
          AND NOT EXISTS (
              SELECT 1 FROM session_members sm WHERE sm.frame_id = fr.id
          )
        GROUP BY COALESCE(fr.object, 'Unknown'), ROUND(fr.ra, 1), ROUND(fr.dec, 1)
        HAVING avg_ra IS NOT NULL AND avg_dec IS NOT NULL

        -- Safety cap: 5000 distinct imaging locations is more than any
        -- realistic library has (typical user: <500). Without this, a
        -- pathological 100K+ frame catalog would ship a multi-MB JSON
        -- payload to the frontend in one hit. T1-9.
        LIMIT 5000
    ").map_err(|e| format!("Failed to prepare query: {}", e))?;

    let locations = stmt.query_map([], |row| {
        let frame_set_id: Option<i64> = row.get(0)?;
        let object_name: Option<String> = row.get(1)?;
        let ra: f64 = row.get(2)?;
        let dec: f64 = row.get(3)?;
        let frame_count: i32 = row.get(4)?;
        let total_exposure: f64 = row.get(5)?;
        let filters_str: Option<String> = row.get(6)?;
        let first_date: Option<String> = row.get(7)?;
        let last_date: Option<String> = row.get(8)?;
        let avg_xpixsz: Option<f64> = row.get(9)?;
        let avg_ypixsz: Option<f64> = row.get(10)?;
        let avg_focallen: Option<f64> = row.get(11)?;
        let avg_naxis1: Option<f64> = row.get(12)?;
        let avg_naxis2: Option<f64> = row.get(13)?;
        let avg_xbinning: Option<f64> = row.get(14)?;
        let avg_ybinning: Option<f64> = row.get(15)?;
        let location_type: String = row.get(16)?;
        let cameras_str: Option<String> = row.get(17)?;
        let focal_lengths_str: Option<String> = row.get(18)?;
        let is_custom: i64 = row.get(19)?;
        let avg_sin_rot: Option<f64> = row.get(20)?;
        let avg_cos_rot: Option<f64> = row.get(21)?;

        // Compute circular mean of rotation angles via atan2(mean_sin, mean_cos)
        let avg_rotation: Option<f64> = match (avg_sin_rot, avg_cos_rot) {
            (Some(s), Some(c)) => Some(s.atan2(c).to_degrees()),
            _ => None,
        };

        // Parse filters from comma-separated string
        let filters: Vec<String> = filters_str
            .map(|s| s.split(',').map(|f| f.trim().to_string()).collect())
            .unwrap_or_default();

        // Calculate FOV using actual sensor dimensions from FITS metadata
        let fov_width = calculate_fov(
            avg_xpixsz,
            avg_focallen,
            avg_naxis1.map(|n| n.round() as i32),
            avg_xbinning.map(|b| b.round() as i32),
        );

        let fov_height = calculate_fov(
            avg_ypixsz.or(avg_xpixsz),
            avg_focallen,
            avg_naxis2.map(|n| n.round() as i32),
            avg_ybinning.map(|b| b.round() as i32),
        );

        // Use a deterministic ID based on location for clusters
        let id = if let Some(fs_id) = frame_set_id {
            fs_id
        } else {
            // Create a pseudo-ID for clusters based on coordinates
            ((ra.to_bits() as i64) ^ (dec.to_bits() as i64)).abs()
        };

        Ok(ImagingLocation {
            id,
            ra,
            dec,
            object_name,
            frame_count,
            total_exposure,
            filters,
            date_range: (
                first_date.unwrap_or_default(),
                last_date.unwrap_or_default()
            ),
            frame_set_id,
            fov_width,
            fov_height,
            location_type,
            cameras: cameras_str,
            focal_lengths: focal_lengths_str,
            is_custom: is_custom != 0,
            rotation: avg_rotation,
        })
    }).map_err(|e| format!("Failed to query imaging locations: {}", e))?;

    let result: Vec<ImagingLocation> = locations
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Failed to collect results: {}", e))?;

    println!("Found {} imaging locations ({} framesets, {} clusters)",
        result.len(),
        result.iter().filter(|l| l.location_type == "frameset").count(),
        result.iter().filter(|l| l.location_type == "cluster").count()
    );

    Ok(result)
}

/// Query candidate frames within a rectangular region of the sky
///
/// Returns frame coordinates so the frontend can do precise pixel-space verification.
/// This is important because RA/Dec bounding boxes break near the poles where a small
/// visual rectangle can span nearly 360° of RA.
///
/// # Arguments
/// * `bounds` - SelectionBounds with ra_min, ra_max, dec_min, dec_max, crosses_meridian
///
/// # Returns
/// SelectionCandidates with frame IDs, coordinates, and exposure times
#[tauri::command(rename_all = "snake_case")]
pub async fn query_frames_in_bounds(
    state: State<'_, AppState>,
    bounds: SelectionBounds,
) -> Result<SelectionCandidates, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Handle RA wrap-around at 0°/360° boundary
    let ra_wrap_around = bounds.crosses_meridian.unwrap_or_else(|| bounds.ra_min > bounds.ra_max);

    println!(
        "Querying frames in bounds: ra_min={}, ra_max={}, dec_min={}, dec_max={}, crosses_meridian={}",
        bounds.ra_min, bounds.ra_max, bounds.dec_min, bounds.dec_max, ra_wrap_around
    );

    let query = if ra_wrap_around {
        "SELECT id, ra, dec, COALESCE(exptime, 0) FROM frames
         WHERE ra IS NOT NULL AND dec IS NOT NULL AND imagetyp = 'Light'
         AND (ra >= ?1 OR ra <= ?2) AND dec BETWEEN ?3 AND ?4"
    } else {
        "SELECT id, ra, dec, COALESCE(exptime, 0) FROM frames
         WHERE ra IS NOT NULL AND dec IS NOT NULL AND imagetyp = 'Light'
         AND ra BETWEEN ?1 AND ?2 AND dec BETWEEN ?3 AND ?4"
    };

    let mut stmt = conn
        .prepare(query)
        .map_err(|e| e.to_string())?;

    let candidates: Vec<SelectionCandidate> = stmt
        .query_map(
            rusqlite::params![bounds.ra_min, bounds.ra_max, bounds.dec_min, bounds.dec_max],
            |row| {
                Ok(SelectionCandidate {
                    id: row.get(0)?,
                    ra: row.get(1)?,
                    dec: row.get(2)?,
                    exposure: row.get(3)?,
                })
            },
        )
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    let total_exposure: f64 = candidates.iter().map(|c| c.exposure).sum();
    let count = candidates.len();

    println!("Found {} candidate frames (ra_wrap_around={})", count, ra_wrap_around);

    Ok(SelectionCandidates {
        candidates,
        count,
        total_exposure_seconds: total_exposure,
    })
}


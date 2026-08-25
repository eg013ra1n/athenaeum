//! Spatial query layer — sky coordinates and location-based operations.
//!
//! Single source of truth for the sky-map queries: the Tauri commands
//! (`commands/spatial.rs`) and the Axum routes (`routes/spatial.rs`) are thin
//! delegations onto the functions below. The SQL is carried over verbatim from
//! the pre-move Tauri copy.

use anyhow::{anyhow, Result};
use rusqlite::Connection;

use crate::models::{ImagingLocation, SelectionBounds, SelectionCandidate, SelectionCandidates};

/// Calculate field of view (FOV) in degrees given camera and telescope parameters
///
/// # Arguments
/// * `pixel_size_um` - Pixel size in micrometers
/// * `focal_length_mm` - Telescope focal length in millimeters
/// * `naxis` - Number of pixels along sensor dimension
/// * `binning` - Binning factor (1 = no binning, 2 = 2x2 binning, etc.)
///
/// # Returns
/// FOV in degrees, or None if parameters are invalid
pub fn calculate_fov(
    pixel_size_um: Option<f64>,
    focal_length_mm: Option<f64>,
    naxis: Option<i32>,
    _binning: Option<i32>,
) -> Option<f64> {
    match (pixel_size_um, focal_length_mm, naxis) {
        (Some(pixel_size), Some(focal_len), Some(sensor_pixels))
            if focal_len > 0.0 && sensor_pixels > 0 =>
        {
            // Convert pixel size from micrometers to millimeters
            let pixel_size_mm = pixel_size / 1000.0;

            // Calculate sensor dimension in mm
            // FITS XPIXSZ reports effective pixel size after binning, so no need to multiply by bin
            let sensor_mm = pixel_size_mm * sensor_pixels as f64;

            // FOV formula: FOV = 2 * arctan(sensor_mm / (2 * focal_length_mm)) * (180 / π)
            let fov_radians = 2.0 * (sensor_mm / (2.0 * focal_len)).atan();
            let fov_degrees = fov_radians.to_degrees();

            Some(fov_degrees)
        }
        _ => None,
    }
}

/// Get all imaging locations (both organized frame sets and unorganized clusters)
///
/// Returns a list of all locations where frames have been taken, including:
/// - Organized locations: Frames that are part of frame sets
/// - Unorganized locations: Frames not in any set, clustered by sky coordinates
pub fn get_imaging_locations(conn: &Connection) -> Result<Vec<ImagingLocation>> {
    // Query both organized frame sets AND unorganized frames
    // This enables users to see all frames with coordinates immediately,
    // without needing to auto-generate frame sets first
    let mut stmt = conn
        .prepare(
            "
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
    ",
        )
        .map_err(|e| anyhow!("Failed to prepare query: {}", e))?;

    let locations = stmt
        .query_map([], |row| {
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
                    last_date.unwrap_or_default(),
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
        })
        .map_err(|e| anyhow!("Failed to query imaging locations: {}", e))?;

    let result: Vec<ImagingLocation> = locations
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| anyhow!("Failed to collect results: {}", e))?;

    tracing::debug!(
        count = result.len(),
        framesets = result
            .iter()
            .filter(|l| l.location_type == "frameset")
            .count(),
        clusters = result
            .iter()
            .filter(|l| l.location_type == "cluster")
            .count(),
        "imaging locations queried"
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
pub fn query_frames_in_bounds(
    conn: &Connection,
    bounds: SelectionBounds,
) -> Result<SelectionCandidates> {
    // Handle RA wrap-around at 0°/360° boundary
    let ra_wrap_around = bounds
        .crosses_meridian
        .unwrap_or_else(|| bounds.ra_min > bounds.ra_max);

    tracing::debug!(
        ra_min = bounds.ra_min,
        ra_max = bounds.ra_max,
        dec_min = bounds.dec_min,
        dec_max = bounds.dec_max,
        ra_wrap_around,
        "querying frames in bounds"
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

    let mut stmt = conn.prepare(query).map_err(|e| anyhow!("{}", e))?;

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
        .map_err(|e| anyhow!("{}", e))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| anyhow!("{}", e))?;

    let total_exposure: f64 = candidates.iter().map(|c| c.exposure).sum();
    let count = candidates.len();

    tracing::debug!(count, ra_wrap_around, "frames in bounds queried");

    Ok(SelectionCandidates {
        candidates,
        count,
        total_exposure_seconds: total_exposure,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;
    use crate::models::SelectionBounds;
    use rusqlite::{params, Connection};

    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        // SIN/COS are per-connection scalar functions in production (bundled
        // SQLite has no math builtins) — install them so the rotation
        // circular-mean columns behave exactly as they do against the pool.
        crate::db::register_math_functions(&conn).unwrap();
        conn
    }

    /// Seed one cataloged LIGHT frame (files row + frames row), mirroring the
    /// columns the spatial queries actually read.
    fn insert_light(
        conn: &Connection,
        id: i64,
        object: &str,
        date_obs: &str,
        ra: f64,
        dec: f64,
        exptime: f64,
    ) {
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, ?3, 1024, '2026-01-01T00:00:00Z', 'FITS')",
            params![
                id,
                format!("/tmp/frame_{id}.fits"),
                format!("frame_{id}.fits")
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames
             (id, file_id, object, date_obs, imagetyp, exptime, filter, ra, dec,
              instrume, focallen, xpixsz, ypixsz, naxis1, naxis2, xbinning, ybinning, rotation)
             VALUES (?1, ?1, ?2, ?3, 'Light', ?4, 'L', ?5, ?6,
                     'TestCam', 200.0, 4.63, 4.63, 4144, 2822, 1, 1, 0.0)",
            params![id, object, date_obs, exptime, ra, dec],
        )
        .unwrap();
    }

    #[test]
    fn imaging_locations_cluster_unorganized_frames() {
        let conn = test_db();
        insert_light(&conn, 1, "M31", "2026-03-05T22:10:00", 10.0, 41.0, 300.0);
        insert_light(&conn, 2, "M31", "2026-03-05T22:20:00", 10.02, 41.01, 300.0);

        let locations = get_imaging_locations(&conn).unwrap();

        assert_eq!(
            locations.len(),
            1,
            "same object + same rounded coords = one cluster"
        );
        let loc = &locations[0];
        assert_eq!(loc.location_type, "cluster");
        assert_eq!(loc.frame_set_id, None);
        assert_eq!(loc.frame_count, 2);
        assert_eq!(loc.total_exposure, 600.0);
        assert_eq!(loc.object_name.as_deref(), Some("M31"));
        assert!((loc.ra - 10.01).abs() < 1e-6, "ra averaged: {}", loc.ra);
        assert!(loc.fov_width.is_some(), "FOV computed from sensor metadata");
    }

    #[test]
    fn frames_in_bounds_finds_frames_inside_window() {
        let conn = test_db();
        insert_light(&conn, 1, "M31", "2026-03-05T22:10:00", 10.0, 41.0, 300.0);
        insert_light(&conn, 2, "M31", "2026-03-05T22:20:00", 10.5, 41.5, 120.0);

        let inside = query_frames_in_bounds(
            &conn,
            SelectionBounds {
                ra_min: 5.0,
                ra_max: 15.0,
                dec_min: 40.0,
                dec_max: 42.0,
                crosses_meridian: None,
            },
        )
        .unwrap();
        assert_eq!(inside.count, 2);
        assert_eq!(inside.candidates.len(), 2);
        assert_eq!(inside.total_exposure_seconds, 420.0);

        let outside = query_frames_in_bounds(
            &conn,
            SelectionBounds {
                ra_min: 100.0,
                ra_max: 110.0,
                dec_min: 40.0,
                dec_max: 42.0,
                crosses_meridian: None,
            },
        )
        .unwrap();
        assert_eq!(outside.count, 0);
    }

    #[test]
    fn frames_in_bounds_honors_meridian_wrap() {
        let conn = test_db();
        insert_light(&conn, 1, "Wrap", "2026-03-05T22:10:00", 359.0, 0.5, 60.0);
        insert_light(&conn, 2, "Wrap", "2026-03-05T22:20:00", 1.0, 0.5, 60.0);
        insert_light(&conn, 3, "Away", "2026-03-05T22:30:00", 180.0, 0.5, 60.0);

        // min_ra > max_ra ⇒ wrap inferred, both sides of 0° match.
        let wrapped = query_frames_in_bounds(
            &conn,
            SelectionBounds {
                ra_min: 350.0,
                ra_max: 10.0,
                dec_min: -1.0,
                dec_max: 1.0,
                crosses_meridian: None,
            },
        )
        .unwrap();
        assert_eq!(
            wrapped.count, 2,
            "straddling frames found across the 0°/360° seam"
        );
        let mut ids: Vec<i64> = wrapped.candidates.iter().map(|c| c.id).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![1, 2]);

        // Explicit crosses_meridian = false forces the plain BETWEEN branch,
        // which cannot match anything when min > max.
        let not_wrapped = query_frames_in_bounds(
            &conn,
            SelectionBounds {
                ra_min: 350.0,
                ra_max: 10.0,
                dec_min: -1.0,
                dec_max: 1.0,
                crosses_meridian: Some(false),
            },
        )
        .unwrap();
        assert_eq!(not_wrapped.count, 0);
    }

    // ── calculate_fov (moved here with its implementation) ────────────────

    #[test]
    fn test_calculate_fov() {
        // Test with typical values (ASI294MC Pro with 200mm focal length)
        let fov = calculate_fov(Some(4.63), Some(200.0), Some(4144), Some(1));
        assert!(fov.is_some());
        let fov_val = fov.unwrap();
        assert!(fov_val > 5.0 && fov_val < 6.0); // Should be ~5.49 degrees
    }

    #[test]
    fn test_calculate_fov_none() {
        assert_eq!(calculate_fov(None, Some(200.0), Some(4144), Some(1)), None);
        assert_eq!(calculate_fov(Some(4.63), None, Some(4144), Some(1)), None);
        assert_eq!(calculate_fov(Some(4.63), Some(200.0), None, Some(1)), None);
    }
}

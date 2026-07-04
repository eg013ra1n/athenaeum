// Frame set processor - processes all light frames in a frame set
use crate::models::{Frame, CalibrationTolerance, CalibrationHierarchy, ImageType};
use crate::calibration::hierarchy::{build_complete_hierarchy, store_calibration_hierarchy};
use rusqlite::Connection;
use anyhow::{Result, Context};
use chrono::{DateTime, Utc};
use serde::{Serialize, Deserialize};

/// Progress report for frame set processing
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct ProcessingProgress {
    pub total_frames: usize,
    pub processed_frames: usize,
    pub current_frame_id: Option<i64>,
    pub percent_complete: f64,
}

/// Statistics for completed frame set processing
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct ProcessingStats {
    pub total_frames: i64,
    pub frames_with_full_calibration: i64,
    pub frames_with_partial_calibration: i64,
    pub frames_with_no_calibration: i64,
    pub total_flat_sets_linked: i64,
    pub total_dark_sets_linked: i64,
    pub total_warnings: i64,
    pub date_warnings: i64,
    pub temp_warnings: i64,
    pub missing_flats: i64,
    pub missing_darks: i64,
    pub missing_bias: i64,
    pub frames_with_flats_only: i64,
    pub frames_with_darks_only: i64,
}

impl ProcessingStats {
    pub fn new() -> Self {
        ProcessingStats {
            total_frames: 0,
            frames_with_full_calibration: 0,
            frames_with_partial_calibration: 0,
            frames_with_no_calibration: 0,
            total_flat_sets_linked: 0,
            total_dark_sets_linked: 0,
            total_warnings: 0,
            date_warnings: 0,
            temp_warnings: 0,
            missing_flats: 0,
            missing_darks: 0,
            missing_bias: 0,
            frames_with_flats_only: 0,
            frames_with_darks_only: 0,
        }
    }

    /// Update statistics based on a calibration hierarchy
    pub fn update_from_hierarchy(&mut self, hierarchy: &CalibrationHierarchy) {
        self.total_frames += 1;

        // Count linked sets
        self.total_flat_sets_linked += hierarchy.flat_sets.len() as i64;
        self.total_dark_sets_linked += hierarchy.dark_sets.len() as i64;

        // Determine calibration completeness
        let has_flat = !hierarchy.flat_sets.is_empty();
        let has_dark = !hierarchy.dark_sets.is_empty();

        if has_flat && has_dark {
            self.frames_with_full_calibration += 1;
        } else if has_flat || has_dark {
            self.frames_with_partial_calibration += 1;
            if has_flat && !has_dark {
                self.frames_with_flats_only += 1;
            }
            if !has_flat && has_dark {
                self.frames_with_darks_only += 1;
            }
        } else {
            self.frames_with_no_calibration += 1;
        }

        // Count warnings
        for warning in &hierarchy.warnings {
            self.total_warnings += 1;
            match warning.warning_type.as_str() {
                "date" => self.date_warnings += 1,
                "temperature" => self.temp_warnings += 1,
                _ => {}
            }
        }

        // Count missing calibration
        // Use exact matches to avoid counting sub-calibration messages incorrectly
        // e.g., "Dark/Bias for Flat" should not count as missing Flat
        for missing in &hierarchy.missing_calibration {
            if missing == "Flat" {
                self.missing_flats += 1;
            }
            if missing == "Dark" {
                self.missing_darks += 1;
            }
            // "Dark/Bias for Flat" means Flat's sub-calibration is missing
            // (Bias is only used as fallback for calibrating Flats when Dark isn't available)
            if missing == "Dark/Bias for Flat" {
                self.missing_bias += 1;
            }
        }
    }
}

/// Get all light frames from a frame set
pub fn get_light_frames_from_frame_set(
    conn: &Connection,
    frame_set_id: i64,
) -> Result<Vec<Frame>> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.file_id, f.object, f.date_obs, f.telescop, f.instrume,
                f.exptime, f.filter, f.imagetyp, f.is_master, f.ra, f.dec, f.objctra, f.objctdec,
                f.gain, f.offset, f.xbinning, f.ybinning, f.ccd_temp, f.set_temp,
                f.focallen, f.xpixsz, f.ypixsz, f.naxis1, f.naxis2, f.sitelat, f.lat_obs, f.sitelong, f.long_obs,
                f.uuid, f.updated_at
         FROM frames f
         JOIN session_members sm ON f.id = sm.frame_id
         JOIN sessions s ON sm.session_id = s.id
         JOIN imaging_nights n ON s.imaging_night_id = n.id
         WHERE n.frames_set_id = ?1
         AND f.imagetyp = 'Light'
         ORDER BY f.date_obs"
    )?;

    let frames = stmt.query_map([frame_set_id], |row| {
        // Parse date_obs as string first, then convert to DateTime
        let date_obs_str: Option<String> = row.get(3)?;
        let date_obs = date_obs_str.and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&Utc));

        // Parse imagetyp as string
        let imagetyp_str: Option<String> = row.get(8)?;
        let imagetyp = imagetyp_str.and_then(|s| ImageType::from_str(&s));

        // Calculate binning string
        let xbinning: Option<i32> = row.get(16)?;
        let ybinning: Option<i32> = row.get(17)?;
        let binning = match (xbinning, ybinning) {
            (Some(x), Some(y)) => Some(format!("{}x{}", x, y)),
            _ => None,
        };

        Ok(Frame {
            id: Some(row.get(0)?),
            file_id: row.get(1)?,
            object: row.get(2)?,
            date_obs,
            telescop: row.get(4)?,
            instrume: row.get(5)?,
            exptime: row.get(6)?,
            filter: row.get(7)?,
            imagetyp,
            is_master: row.get(9)?,
            ra: row.get(10)?,
            dec: row.get(11)?,
            objctra: row.get(12)?,
            objctdec: row.get(13)?,
            gain: row.get(14)?,
            offset: row.get(15)?,
            xbinning,
            ybinning,
            binning,
            ccd_temp: row.get(18)?,
            set_temp: row.get(19)?,
            focallen: row.get(20)?,
            xpixsz: row.get(21)?,
            ypixsz: row.get(22)?,
            naxis1: row.get(23)?,
            naxis2: row.get(24)?,
            sitelat: row.get(25)?,
            lat_obs: row.get(26)?,
            sitelong: row.get(27)?,
            long_obs: row.get(28)?,
            override_: false,
            swcreate: None,
            bayerpat: None,
            rotation: None,
            uuid: row.get(29)?,
            updated_at: row.get(30)?,
        })
    })?
    .collect::<Result<Vec<Frame>, _>>()?;

    Ok(frames)
}

/// Process all light frames in a frame set and find calibration for each
///
/// # Arguments
/// * `conn` - Database connection
/// * `frame_set_id` - ID of the frame set to process
/// * `tolerance` - Calibration matching tolerance
/// * `flat_pattern` - Optional flat pattern preference (e.g., "automatic", "long_term", "manual")
/// * `manual_flat_selections` - Optional manual flat selections per filter
/// * `max_age_days` - Maximum age of flats to consider
/// * `time_cluster_minutes` - Time threshold for grouping flats
/// * `temp_weight` - Weight for temperature matching
pub fn process_frame_set(
    conn: &Connection,
    frame_set_id: i64,
    tolerance: &CalibrationTolerance,
    flat_pattern: Option<&str>,
    manual_flat_selections: Option<&std::collections::HashMap<String, i64>>,
    max_age_days: i64,
    time_cluster_minutes: i64,
    temp_weight: f64,
) -> Result<ProcessingStats> {
    // Get all light frames from the frame set
    let frames = get_light_frames_from_frame_set(conn, frame_set_id)
        .context("Failed to get light frames from frame set")?;

    let mut stats = ProcessingStats::new();
    let total_frames = frames.len();

    // Process each frame
    for (index, frame) in frames.iter().enumerate() {
        let (manual_flat_set_id, manual_dark_set_id) =
            resolve_manual_overrides(conn, frame, manual_flat_selections);

        // Build calibration hierarchy for this frame
        let hierarchy = build_complete_hierarchy(
            conn,
            frame,
            tolerance,
            flat_pattern,
            manual_flat_set_id,
            manual_dark_set_id,
            max_age_days,
            time_cluster_minutes,
            temp_weight,
        ).context(format!("Failed to build hierarchy for frame {:?}", frame.id))?;

        // Store hierarchy in database
        store_calibration_hierarchy(conn, &hierarchy)
            .context(format!("Failed to store hierarchy for frame {:?}", frame.id))?;

        // Update statistics
        stats.update_from_hierarchy(&hierarchy);

        // Progress tracking (could be used for callbacks in the future)
        let progress = ProcessingProgress {
            total_frames,
            processed_frames: index + 1,
            current_frame_id: frame.id,
            percent_complete: ((index + 1) as f64 / total_frames as f64) * 100.0,
        };

        // For now, just log progress (in future, this could call a progress callback)
        if (index + 1) % 10 == 0 || index + 1 == total_frames {
            tracing::debug!(
                frame_set_id,
                processed = progress.processed_frames,
                total = progress.total_frames,
                percent_complete = progress.percent_complete,
                "processing frame set: progress"
            );
        }
    }

    Ok(stats)
}

/// Resolve which Flat / Dark calibration set IDs (if any) should be treated as
/// manual overrides for a given frame.
///
/// Precedence:
/// 1. Frontend-provided `manual_flat_selections` (per-filter map) — wins for Flat.
/// 2. DB-stored `is_manual_override = 1` row in `calibration_set_to_frames`.
///
/// Without this fallback, "Find Calibration" runs the auto-matcher even when a
/// user has previously locked a calibration set for a frame, which means the
/// sub-calibration chain (DarkFlat/Dark/Bias for Flat, Bias for Dark) ends up
/// computed against the auto-detected parent and never against the user's pick.
fn resolve_manual_overrides(
    conn: &Connection,
    frame: &Frame,
    manual_flat_selections: Option<&std::collections::HashMap<String, i64>>,
) -> (Option<i64>, Option<i64>) {
    use crate::db::calibration_links::get_manual_override_set_id;

    let frontend_flat = manual_flat_selections.and_then(|selections| {
        frame
            .filter
            .as_ref()
            .and_then(|filter| selections.get(filter).copied())
    });

    let frame_id = match frame.id {
        Some(id) => id,
        None => return (frontend_flat, None),
    };

    let manual_flat_set_id = frontend_flat
        .or_else(|| get_manual_override_set_id(conn, frame_id, "Flat").ok().flatten());

    let manual_dark_set_id = get_manual_override_set_id(conn, frame_id, "Dark")
        .ok()
        .flatten();

    (manual_flat_set_id, manual_dark_set_id)
}

/// Process all light frames in a frame set with progress callback
#[allow(dead_code)]
pub fn process_frame_set_with_progress<F>(
    conn: &Connection,
    frame_set_id: i64,
    tolerance: &CalibrationTolerance,
    flat_pattern: Option<&str>,
    manual_flat_selections: Option<&std::collections::HashMap<String, i64>>,
    max_age_days: i64,
    time_cluster_minutes: i64,
    temp_weight: f64,
    mut progress_callback: F,
) -> Result<ProcessingStats>
where
    F: FnMut(ProcessingProgress),
{
    // Get all light frames from the frame set
    let frames = get_light_frames_from_frame_set(conn, frame_set_id)
        .context("Failed to get light frames from frame set")?;

    let mut stats = ProcessingStats::new();
    let total_frames = frames.len();

    // Process each frame
    for (index, frame) in frames.iter().enumerate() {
        let (manual_flat_set_id, manual_dark_set_id) =
            resolve_manual_overrides(conn, frame, manual_flat_selections);

        // Build calibration hierarchy for this frame
        let hierarchy = build_complete_hierarchy(
            conn,
            frame,
            tolerance,
            flat_pattern,
            manual_flat_set_id,
            manual_dark_set_id,
            max_age_days,
            time_cluster_minutes,
            temp_weight,
        ).context(format!("Failed to build hierarchy for frame {:?}", frame.id))?;

        // Store hierarchy in database
        store_calibration_hierarchy(conn, &hierarchy)
            .context(format!("Failed to store hierarchy for frame {:?}", frame.id))?;

        // Update statistics
        stats.update_from_hierarchy(&hierarchy);

        // Report progress
        let progress = ProcessingProgress {
            total_frames,
            processed_frames: index + 1,
            current_frame_id: frame.id,
            percent_complete: ((index + 1) as f64 / total_frames as f64) * 100.0,
        };

        progress_callback(progress);
    }

    Ok(stats)
}

/// Clear all calibration links for a frame set
///
/// By default, this preserves manual overrides. Set `preserve_manual_overrides = false`
/// to clear all links including manual ones.
pub fn clear_calibration_links_for_frame_set(
    conn: &Connection,
    frame_set_id: i64,
) -> Result<usize> {
    clear_calibration_links_for_frame_set_with_options(conn, frame_set_id, true)
}

/// Clear calibration links for a frame set with options
///
/// # Arguments
/// * `preserve_manual_overrides` - If true, manual overrides are kept
pub fn clear_calibration_links_for_frame_set_with_options(
    conn: &Connection,
    frame_set_id: i64,
    preserve_manual_overrides: bool,
) -> Result<usize> {
    // Get all frame IDs in the frame set
    let mut stmt = conn.prepare(
        "SELECT DISTINCT sm.frame_id
         FROM session_members sm
         JOIN sessions s ON sm.session_id = s.id
         JOIN imaging_nights n ON s.imaging_night_id = n.id
         WHERE n.frames_set_id = ?1"
    )?;

    let frame_ids: Vec<i64> = stmt
        .query_map([frame_set_id], |row| row.get(0))?
        .collect::<Result<Vec<i64>, _>>()?;

    // Delete calibration links for each frame
    let mut total_deleted = 0;
    for frame_id in frame_ids {
        let deleted = if preserve_manual_overrides {
            // Only delete non-manual links
            conn.execute(
                "DELETE FROM calibration_set_to_frames
                 WHERE source_id = ?1 AND source_type = 'frame' AND is_manual_override = 0",
                [frame_id],
            )?
        } else {
            // Delete all links
            conn.execute(
                "DELETE FROM calibration_set_to_frames
                 WHERE source_id = ?1 AND source_type = 'frame'",
                [frame_id],
            )?
        };
        total_deleted += deleted;
    }

    if preserve_manual_overrides {
        tracing::info!(frame_set_id, count = total_deleted, "cleared auto-find calibration links, manual overrides preserved");
    } else {
        tracing::info!(frame_set_id, count = total_deleted, "cleared all calibration links, including manual");
    }

    Ok(total_deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_processing_stats_initialization() {
        let stats = ProcessingStats::new();
        assert_eq!(stats.total_frames, 0);
        assert_eq!(stats.frames_with_full_calibration, 0);
        assert_eq!(stats.total_warnings, 0);
    }

    #[test]
    fn test_progress_calculation() {
        let progress = ProcessingProgress {
            total_frames: 100,
            processed_frames: 50,
            current_frame_id: Some(123),
            percent_complete: 50.0,
        };

        assert_eq!(progress.percent_complete, 50.0);
        assert_eq!(progress.processed_frames, 50);
    }

    #[test]
    fn test_stats_update_full_calibration() {
        use crate::models::CalibrationSetWithLinks;

        let mut stats = ProcessingStats::new();

        let hierarchy = CalibrationHierarchy {
            light_frame_id: 1,
            flat_sets: vec![CalibrationSetWithLinks {
                set: crate::models::CalibrationSetDetail {
                    id: Some(1),
                    imagetyp: crate::models::ImageType::Flat,
                    exptime: None,
                    filter: Some("L".to_string()),
                    ccd_temp: -10.0,
                    gain: Some(100.0),
                    offset: Some(10.0),
                    binning: Some("1x1".to_string()),
                    instrume: Some("ASI2600MM".to_string()),
                    date_display: "2025-01".to_string(),
                    date_start: "2025-01-15T00:00:00Z".to_string(),
                    date_end: "2025-01-15T23:59:59Z".to_string(),
                    temp_min: -10.5,
                    temp_max: -9.5,
                    frame_count: 10,
                    is_master: false,
                    naxis1: None,
                    naxis2: None,
                    bayerpat: None,
                    swcreate: None,
                    xpixsz: None,
                    format: None,
                    focallen: None,
                    uuid: None,
                    updated_at: None,
                    superseded_by_set_id: None,
                },
                sub_calibration: vec![],
            }],
            dark_sets: vec![CalibrationSetWithLinks {
                set: crate::models::CalibrationSetDetail {
                    id: Some(2),
                    imagetyp: crate::models::ImageType::Dark,
                    exptime: Some(300.0),
                    filter: None,
                    ccd_temp: -10.0,
                    gain: Some(100.0),
                    offset: Some(10.0),
                    binning: Some("1x1".to_string()),
                    instrume: Some("ASI2600MM".to_string()),
                    date_display: "2025-01".to_string(),
                    date_start: "2025-01-15T00:00:00Z".to_string(),
                    date_end: "2025-01-15T23:59:59Z".to_string(),
                    temp_min: -10.5,
                    temp_max: -9.5,
                    frame_count: 20,
                    is_master: false,
                    naxis1: None,
                    naxis2: None,
                    bayerpat: None,
                    swcreate: None,
                    xpixsz: None,
                    format: None,
                    focallen: None,
                    uuid: None,
                    updated_at: None,
                    superseded_by_set_id: None,
                },
                sub_calibration: vec![],
            }],
            missing_calibration: vec![],
            warnings: vec![],
        };

        stats.update_from_hierarchy(&hierarchy);

        assert_eq!(stats.total_frames, 1);
        assert_eq!(stats.frames_with_full_calibration, 1);
        assert_eq!(stats.frames_with_flats_only, 0);
        assert_eq!(stats.frames_with_darks_only, 0);
        assert_eq!(stats.total_flat_sets_linked, 1);
        assert_eq!(stats.total_dark_sets_linked, 1);
    }
}

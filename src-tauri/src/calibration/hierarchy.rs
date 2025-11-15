// Calibration hierarchy builder - constructs complete calibration trees
use crate::calibration::finder::{
    find_flat_sets_for_light_frame, find_dark_sets_for_light_frame,
    find_bias_sets_for_frame, rank_calibration_candidates, CalibrationCandidate,
};
use crate::db::calibration_links::insert_calibration_link;
use crate::models::{
    CalibrationTolerance, Frame, CalibrationLink, CalibrationWarning,
    CalibrationHierarchy, CalibrationSetWithLinks, CalibrationSetDetail, ImageType,
};
use rusqlite::Connection;
use anyhow::{Result, Context};
use chrono::{Utc, DateTime};

/// Get frame metadata by ID
fn get_frame_by_id(conn: &Connection, frame_id: i64) -> Result<Frame> {
    let mut stmt = conn.prepare(
        "SELECT id, file_id, object, date_obs, telescop, instrume, exptime, filter,
                gain, offset, binning, xbinning, ybinning, ccd_temp, set_temp,
                focallen, xpixsz, pixsz, naxis1, naxis2, ra, dec, sitelat, lat_obs,
                sitelong, long_obs, objctra, objctdec, override, imagetyp, is_master
         FROM frames
         WHERE id = ?1"
    )?;

    let frame = stmt.query_row([frame_id], |row| {
        let date_obs_str: Option<String> = row.get(3)?;
        let date_obs = date_obs_str.and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&Utc));

        Ok(Frame {
            id: Some(row.get(0)?),
            file_id: row.get(1)?,
            object: row.get(2)?,
            date_obs,
            telescop: row.get(4)?,
            instrume: row.get(5)?,
            exptime: row.get(6)?,
            filter: row.get(7)?,
            gain: row.get(8)?,
            offset: row.get(9)?,
            binning: row.get(10)?,
            xbinning: row.get(11)?,
            ybinning: row.get(12)?,
            ccd_temp: row.get(13)?,
            set_temp: row.get(14)?,
            focallen: row.get(15)?,
            xpixsz: row.get(16)?,
            pixsz: row.get(17)?,
            naxis1: row.get(18)?,
            naxis2: row.get(19)?,
            ra: row.get(20)?,
            dec: row.get(21)?,
            sitelat: row.get(22)?,
            lat_obs: row.get(23)?,
            sitelong: row.get(24)?,
            long_obs: row.get(25)?,
            objctra: row.get(26)?,
            objctdec: row.get(27)?,
            override_: row.get::<_, i32>(28)? != 0,
            imagetyp: {
                let imagetyp_str: Option<String> = row.get(29)?;
                imagetyp_str.and_then(|s| ImageType::from_str(&s))
            },
            is_master: row.get::<_, i32>(30)? != 0,
        })
    })?;

    Ok(frame)
}

/// Get calibration set detail by ID
fn get_calibration_set_by_id(conn: &Connection, set_id: i64) -> Result<CalibrationSetDetail> {
    let mut stmt = conn.prepare(
        "SELECT id, imagetyp, exptime, ccd_temp, temp_min, temp_max, gain, offset,
                binning, instrume, date_start, date_end, date, frame_count
         FROM calibration_set
         WHERE id = ?1"
    )?;

    let set = stmt.query_row([set_id], |row| {
        let imagetyp_str: String = row.get(1)?;
        Ok(CalibrationSetDetail {
            id: Some(row.get(0)?),
            imagetyp: ImageType::from_str(&imagetyp_str)
                .ok_or_else(|| rusqlite::Error::InvalidQuery)?,
            exptime: row.get(2)?,
            ccd_temp: row.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
            temp_min: row.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
            temp_max: row.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
            gain: row.get(6)?,
            offset: row.get(7)?,
            binning: row.get(8)?,
            instrume: row.get(9)?,
            date_start: row.get(10)?,
            date_end: row.get(11)?,
            date_display: row.get(12)?,
            frame_count: row.get(13)?,
        })
    })?;

    Ok(set)
}

/// Find Dark or Bias calibration for a Flat calibration set
/// First tries to find Dark sets, falls back to Bias if not found
pub fn find_calibration_for_flat_set(
    conn: &Connection,
    flat_set_id: i64,
    tolerance: &CalibrationTolerance,
) -> Result<Vec<CalibrationCandidate>> {
    // Get a representative frame from the flat set to use for matching
    let mut stmt = conn.prepare(
        "SELECT frame_id FROM calibration_set_frames WHERE set_id = ?1 LIMIT 1"
    )?;

    let frame_id: i64 = stmt.query_row([flat_set_id], |row| row.get(0))
        .context("Flat set has no frames")?;

    let frame = get_frame_by_id(conn, frame_id)?;

    // Try to find Dark sets first
    let dark_candidates = find_dark_sets_for_light_frame(conn, &frame, tolerance)?;

    if !dark_candidates.is_empty() {
        return Ok(rank_calibration_candidates(dark_candidates));
    }

    // Fallback to Bias sets
    let bias_candidates = find_bias_sets_for_frame(conn, &frame, tolerance)?;
    Ok(rank_calibration_candidates(bias_candidates))
}

/// Find Bias calibration for a Dark calibration set
pub fn find_calibration_for_dark_set(
    conn: &Connection,
    dark_set_id: i64,
    tolerance: &CalibrationTolerance,
) -> Result<Vec<CalibrationCandidate>> {
    // Get a representative frame from the dark set
    let mut stmt = conn.prepare(
        "SELECT frame_id FROM calibration_set_frames WHERE set_id = ?1 LIMIT 1"
    )?;

    let frame_id: i64 = stmt.query_row([dark_set_id], |row| row.get(0))
        .context("Dark set has no frames")?;

    let frame = get_frame_by_id(conn, frame_id)?;

    // Find Bias sets
    let bias_candidates = find_bias_sets_for_frame(conn, &frame, tolerance)?;
    Ok(rank_calibration_candidates(bias_candidates))
}

/// Build complete calibration hierarchy for a light frame
/// Returns hierarchy including all flats, darks, and their sub-calibrations
pub fn build_complete_hierarchy(
    conn: &Connection,
    light_frame: &Frame,
    tolerance: &CalibrationTolerance,
) -> Result<CalibrationHierarchy> {
    let frame_id = light_frame.id.context("Frame must have an ID")?;

    let mut flat_sets_with_links = Vec::new();
    let mut dark_sets_with_links = Vec::new();
    let mut missing_calibration = Vec::new();
    let mut warnings = Vec::new();

    // Find Flat sets for the light frame
    let flat_candidates = find_flat_sets_for_light_frame(conn, light_frame, tolerance)?;
    let ranked_flats = rank_calibration_candidates(flat_candidates);

    if ranked_flats.is_empty() {
        missing_calibration.push("Flat".to_string());
    } else {
        // Take the best flat match
        if let Some(best_flat) = ranked_flats.first() {
            let flat_set = get_calibration_set_by_id(conn, best_flat.set_id)?;

            // Add warnings for the flat
            if best_flat.date_warning {
                warnings.push(CalibrationWarning {
                    warning_type: "date".to_string(),
                    message: format!(
                        "Flat calibration is {} days old (>{} days recommended)",
                        best_flat.date_diff_days,
                        tolerance.flat_date_warning_days
                    ),
                    calibration_type: "Flat".to_string(),
                    set_id: best_flat.set_id,
                });
            }
            if best_flat.temp_warning {
                warnings.push(CalibrationWarning {
                    warning_type: "temperature".to_string(),
                    message: format!(
                        "Flat temperature differs by {:.1}°C",
                        best_flat.temp_diff.unwrap_or(0.0)
                    ),
                    calibration_type: "Flat".to_string(),
                    set_id: best_flat.set_id,
                });
            }

            // Find calibration for the Flat set (Dark or Bias)
            let flat_calib = find_calibration_for_flat_set(conn, best_flat.set_id, tolerance)?;

            let mut flat_sub_calibration = Vec::new();
            if let Some(best_flat_calib) = flat_calib.first() {
                // Store the calibration link
                flat_sub_calibration.push(CalibrationLink {
                    id: None,
                    source_id: best_flat.set_id,
                    source_type: "calibration_set".to_string(),
                    calibration_set_id: best_flat_calib.set_id,
                    calibration_type: match best_flat_calib.imagetyp {
                        ImageType::Dark => "Dark".to_string(),
                        ImageType::Bias => "Bias".to_string(),
                        _ => "Unknown".to_string(),
                    },
                    matched_at: Utc::now().to_rfc3339(),
                    match_score: Some(best_flat_calib.match_score),
                    date_warning: best_flat_calib.date_warning,
                    temp_warning: best_flat_calib.temp_warning,
                });

                // Add warnings
                if best_flat_calib.date_warning {
                    warnings.push(CalibrationWarning {
                        warning_type: "date".to_string(),
                        message: format!(
                            "{} for Flat is {} days old",
                            match best_flat_calib.imagetyp {
                                ImageType::Dark => "Dark",
                                ImageType::Bias => "Bias",
                                _ => "Calibration",
                            },
                            best_flat_calib.date_diff_days
                        ),
                        calibration_type: match best_flat_calib.imagetyp {
                            ImageType::Dark => "Dark".to_string(),
                            ImageType::Bias => "Bias".to_string(),
                            _ => "Unknown".to_string(),
                        },
                        set_id: best_flat_calib.set_id,
                    });
                }
            } else {
                missing_calibration.push("Dark/Bias for Flat".to_string());
            }

            flat_sets_with_links.push(CalibrationSetWithLinks {
                set: flat_set,
                sub_calibration: flat_sub_calibration,
            });
        }
    }

    // Find Dark sets for the light frame
    let dark_candidates = find_dark_sets_for_light_frame(conn, light_frame, tolerance)?;
    let ranked_darks = rank_calibration_candidates(dark_candidates);

    if ranked_darks.is_empty() {
        missing_calibration.push("Dark".to_string());
    } else {
        // Take the best dark match
        if let Some(best_dark) = ranked_darks.first() {
            let dark_set = get_calibration_set_by_id(conn, best_dark.set_id)?;

            // Add warnings for the dark
            if best_dark.date_warning {
                warnings.push(CalibrationWarning {
                    warning_type: "date".to_string(),
                    message: format!(
                        "Dark calibration is {} days old (>{} days recommended)",
                        best_dark.date_diff_days,
                        tolerance.dark_date_warning_days
                    ),
                    calibration_type: "Dark".to_string(),
                    set_id: best_dark.set_id,
                });
            }
            if best_dark.temp_warning {
                warnings.push(CalibrationWarning {
                    warning_type: "temperature".to_string(),
                    message: format!(
                        "Dark temperature differs by {:.1}°C",
                        best_dark.temp_diff.unwrap_or(0.0)
                    ),
                    calibration_type: "Dark".to_string(),
                    set_id: best_dark.set_id,
                });
            }

            // Find Bias for the Dark set
            let bias_candidates = find_calibration_for_dark_set(conn, best_dark.set_id, tolerance)?;

            let mut dark_sub_calibration = Vec::new();
            if let Some(best_bias) = bias_candidates.first() {
                dark_sub_calibration.push(CalibrationLink {
                    id: None,
                    source_id: best_dark.set_id,
                    source_type: "calibration_set".to_string(),
                    calibration_set_id: best_bias.set_id,
                    calibration_type: "Bias".to_string(),
                    matched_at: Utc::now().to_rfc3339(),
                    match_score: Some(best_bias.match_score),
                    date_warning: best_bias.date_warning,
                    temp_warning: best_bias.temp_warning,
                });

                // Bias doesn't have date warnings, but might have temp warnings
                if best_bias.temp_warning {
                    warnings.push(CalibrationWarning {
                        warning_type: "temperature".to_string(),
                        message: format!(
                            "Bias for Dark temperature differs by {:.1}°C",
                            best_bias.temp_diff.unwrap_or(0.0)
                        ),
                        calibration_type: "Bias".to_string(),
                        set_id: best_bias.set_id,
                    });
                }
            } else {
                missing_calibration.push("Bias for Dark".to_string());
            }

            dark_sets_with_links.push(CalibrationSetWithLinks {
                set: dark_set,
                sub_calibration: dark_sub_calibration,
            });
        }
    }

    Ok(CalibrationHierarchy {
        light_frame_id: frame_id,
        flat_sets: flat_sets_with_links,
        dark_sets: dark_sets_with_links,
        missing_calibration,
        warnings,
    })
}

/// Store calibration hierarchy in database
/// Creates all necessary calibration links
pub fn store_calibration_hierarchy(
    conn: &Connection,
    hierarchy: &CalibrationHierarchy,
) -> Result<()> {
    // Store Flat set links
    for flat_set_with_links in &hierarchy.flat_sets {
        if let Some(set_id) = flat_set_with_links.set.id {
            let link = CalibrationLink {
                id: None,
                source_id: hierarchy.light_frame_id,
                source_type: "frame".to_string(),
                calibration_set_id: set_id,
                calibration_type: "Flat".to_string(),
                matched_at: Utc::now().to_rfc3339(),
                match_score: Some(1.0), // Best match was selected
                date_warning: hierarchy.warnings.iter().any(|w|
                    w.calibration_type == "Flat" && w.warning_type == "date"
                ),
                temp_warning: hierarchy.warnings.iter().any(|w|
                    w.calibration_type == "Flat" && w.warning_type == "temperature"
                ),
            };
            insert_calibration_link(conn, &link)?;

            // Store sub-calibration links (Flat → Dark/Bias)
            for sub_link in &flat_set_with_links.sub_calibration {
                insert_calibration_link(conn, sub_link)?;
            }
        }
    }

    // Store Dark set links
    for dark_set_with_links in &hierarchy.dark_sets {
        if let Some(set_id) = dark_set_with_links.set.id {
            let link = CalibrationLink {
                id: None,
                source_id: hierarchy.light_frame_id,
                source_type: "frame".to_string(),
                calibration_set_id: set_id,
                calibration_type: "Dark".to_string(),
                matched_at: Utc::now().to_rfc3339(),
                match_score: Some(1.0),
                date_warning: hierarchy.warnings.iter().any(|w|
                    w.calibration_type == "Dark" && w.warning_type == "date"
                ),
                temp_warning: hierarchy.warnings.iter().any(|w|
                    w.calibration_type == "Dark" && w.warning_type == "temperature"
                ),
            };
            insert_calibration_link(conn, &link)?;

            // Store sub-calibration links (Dark → Bias)
            for sub_link in &dark_set_with_links.sub_calibration {
                insert_calibration_link(conn, sub_link)?;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hierarchy_structure() {
        // Test that hierarchy can be built with empty sets
        let hierarchy = CalibrationHierarchy {
            light_frame_id: 123,
            flat_sets: Vec::new(),
            dark_sets: Vec::new(),
            missing_calibration: vec!["Flat".to_string(), "Dark".to_string()],
            warnings: Vec::new(),
        };

        assert_eq!(hierarchy.light_frame_id, 123);
        assert_eq!(hierarchy.missing_calibration.len(), 2);
        assert!(hierarchy.flat_sets.is_empty());
        assert!(hierarchy.dark_sets.is_empty());
    }

    #[test]
    fn test_missing_calibration_tracking() {
        let mut missing = Vec::new();

        // Simulate no flats found
        missing.push("Flat".to_string());

        // Simulate no darks found
        missing.push("Dark".to_string());

        // Simulate no bias for dark found
        missing.push("Bias for Dark".to_string());

        assert_eq!(missing.len(), 3);
        assert!(missing.contains(&"Flat".to_string()));
        assert!(missing.contains(&"Dark".to_string()));
        assert!(missing.contains(&"Bias for Dark".to_string()));
    }

    #[test]
    fn test_warning_accumulation() {
        let mut warnings = Vec::new();

        warnings.push(CalibrationWarning {
            warning_type: "date".to_string(),
            message: "Flat is 45 days old".to_string(),
            calibration_type: "Flat".to_string(),
            set_id: 10,
        });

        warnings.push(CalibrationWarning {
            warning_type: "temperature".to_string(),
            message: "Dark temperature differs by 5°C".to_string(),
            calibration_type: "Dark".to_string(),
            set_id: 20,
        });

        assert_eq!(warnings.len(), 2);

        let date_warnings: Vec<_> = warnings.iter()
            .filter(|w| w.warning_type == "date")
            .collect();
        assert_eq!(date_warnings.len(), 1);

        let temp_warnings: Vec<_> = warnings.iter()
            .filter(|w| w.warning_type == "temperature")
            .collect();
        assert_eq!(temp_warnings.len(), 1);
    }
}

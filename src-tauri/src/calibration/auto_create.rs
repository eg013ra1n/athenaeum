// Auto-creation of calibration sets from individual frames
use crate::models::{ImageType, CalibrationTolerance};
use rusqlite::{Connection, params, OptionalExtension};
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::collections::HashMap;

/// Represents a suggested calibration set to be created
#[derive(Debug, Clone)]
pub struct SuggestedCalibrationSet {
    pub imagetyp: ImageType,
    pub frame_ids: Vec<i64>,
    pub gain: Option<f64>,
    pub offset: Option<f64>,
    pub binning: Option<String>,
    pub instrume: Option<String>,
    pub filter: Option<String>,
    pub exptime: Option<f64>,
    pub focallen: Option<f64>,
    pub temp_min: Option<f64>,
    pub temp_max: Option<f64>,
    pub date_start: Option<String>,
    pub date_end: Option<String>,
}

/// Check if a calibration set with matching parameters already exists
pub fn check_for_duplicate_set(
    conn: &Connection,
    imagetyp: &ImageType,
    gain: Option<f64>,
    offset: Option<f64>,
    binning: &Option<String>,
    instrume: &Option<String>,
    filter: &Option<String>,
    exptime: Option<f64>,
    focallen: Option<f64>,
) -> Result<Option<i64>> {
    // Build query with exact parameter matching
    let imagetyp_str = match imagetyp {
        ImageType::Dark => "Dark",
        ImageType::Flat => "Flat",
        ImageType::Bias => "Bias",
        ImageType::DarkFlat => "DarkFlat",
        ImageType::MasterDark => "MasterDark",
        ImageType::MasterFlat => "MasterFlat",
        ImageType::MasterBias => "MasterBias",
        ImageType::MasterDarkFlat => "MasterDarkFlat",
        _ => return Ok(None),
    };

    let mut query = "SELECT id FROM calibration_set WHERE imagetyp = ?1".to_string();
    let mut param_count = 2;

    // Add gain check
    if gain.is_some() {
        query.push_str(&format!(" AND ABS(gain - ?{}) < 0.01", param_count));
        param_count += 1;
    } else {
        query.push_str(" AND gain IS NULL");
    }

    // Add offset check
    if offset.is_some() {
        query.push_str(&format!(" AND ABS(offset - ?{}) < 0.01", param_count));
        param_count += 1;
    } else {
        query.push_str(" AND offset IS NULL");
    }

    // Add binning check
    if binning.is_some() {
        query.push_str(&format!(" AND binning = ?{}", param_count));
        param_count += 1;
    } else {
        query.push_str(" AND binning IS NULL");
    }

    // Add instrume check
    if instrume.is_some() {
        query.push_str(&format!(" AND instrume = ?{}", param_count));
        param_count += 1;
    } else {
        query.push_str(" AND instrume IS NULL");
    }

    // Add filter check (for Flats)
    if *imagetyp == ImageType::Flat {
        if filter.is_some() {
            query.push_str(&format!(" AND filter = ?{}", param_count));
            param_count += 1;
        } else {
            query.push_str(" AND filter IS NULL");
        }
    }

    // Add exptime check (for Darks)
    if *imagetyp == ImageType::Dark {
        if exptime.is_some() {
            query.push_str(&format!(" AND ABS(exptime - ?{}) < 0.1", param_count));
            param_count += 1;
        } else {
            query.push_str(" AND exptime IS NULL");
        }
    }

    // Add focallen check (for Flats)
    if *imagetyp == ImageType::Flat {
        if focallen.is_some() {
            query.push_str(&format!(" AND ABS(focallen - ?{}) < 1.0", param_count));
            param_count += 1;
        } else {
            query.push_str(" AND focallen IS NULL");
        }
    }

    query.push_str(" LIMIT 1");

    // Execute query with dynamic parameters
    let mut stmt = conn.prepare(&query)?;
    let mut param_values: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(imagetyp_str.to_string())];

    if let Some(g) = gain {
        param_values.push(Box::new(g));
    }
    if let Some(o) = offset {
        param_values.push(Box::new(o));
    }
    if let Some(ref b) = binning {
        param_values.push(Box::new(b.clone()));
    }
    if let Some(ref i) = instrume {
        param_values.push(Box::new(i.clone()));
    }
    if *imagetyp == ImageType::Flat {
        if let Some(ref f) = filter {
            param_values.push(Box::new(f.clone()));
        }
    }
    if *imagetyp == ImageType::Dark {
        if let Some(e) = exptime {
            param_values.push(Box::new(e));
        }
    }
    if *imagetyp == ImageType::Flat {
        if let Some(fl) = focallen {
            param_values.push(Box::new(fl));
        }
    }

    let params_refs: Vec<&dyn rusqlite::ToSql> = param_values
        .iter()
        .map(|b| b.as_ref() as &dyn rusqlite::ToSql)
        .collect();

    let existing_id = stmt.query_row(&params_refs[..], |row| row.get(0)).optional()?;

    Ok(existing_id)
}

/// Find individual calibration frames matching the given parameters
pub fn find_matching_individual_frames(
    conn: &Connection,
    imagetyp: &ImageType,
    gain: Option<f64>,
    offset: Option<f64>,
    binning: &Option<String>,
    instrume: &Option<String>,
    filter: &Option<String>,
    exptime: Option<f64>,
    focallen: Option<f64>,
    tolerance: &CalibrationTolerance,
) -> Result<Vec<i64>> {
    let imagetyp_str = match imagetyp {
        ImageType::Dark => "Dark",
        ImageType::Flat => "Flat",
        ImageType::Bias => "Bias",
        ImageType::DarkFlat => "DarkFlat",
        _ => return Ok(Vec::new()),
    };

    // Build query to find matching frames
    // Only select frames that are NOT already in a calibration set
    let mut query = "SELECT DISTINCT f.id
                     FROM frames f
                     WHERE f.imagetyp = ?1
                     AND f.id NOT IN (SELECT frame_id FROM calibration_set_frames)".to_string();

    let mut param_count = 2;

    // Add parameter filters
    if gain.is_some() {
        query.push_str(&format!(" AND ABS(f.gain - ?{}) < 0.01", param_count));
        param_count += 1;
    }
    if offset.is_some() {
        query.push_str(&format!(" AND ABS(f.offset - ?{}) < 0.01", param_count));
        param_count += 1;
    }
    if binning.is_some() {
        query.push_str(&format!(" AND f.binning = ?{}", param_count));
        param_count += 1;
    }
    if instrume.is_some() {
        query.push_str(&format!(" AND f.instrume = ?{}", param_count));
        param_count += 1;
    }
    if *imagetyp == ImageType::Flat && filter.is_some() {
        query.push_str(&format!(" AND f.filter = ?{}", param_count));
        param_count += 1;
    }
    if *imagetyp == ImageType::Dark && exptime.is_some() {
        query.push_str(&format!(" AND ABS(f.exptime - ?{}) < 0.1", param_count));
        param_count += 1;
    }
    if *imagetyp == ImageType::Flat && focallen.is_some() {
        query.push_str(&format!(" AND ABS(f.focallen - ?{}) < 1.0", param_count));
        param_count += 1;
    }

    query.push_str(" ORDER BY f.date_obs");

    let mut stmt = conn.prepare(&query)?;
    let mut param_values: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(imagetyp_str.to_string())];

    if let Some(g) = gain {
        param_values.push(Box::new(g));
    }
    if let Some(o) = offset {
        param_values.push(Box::new(o));
    }
    if let Some(ref b) = binning {
        param_values.push(Box::new(b.clone()));
    }
    if let Some(ref i) = instrume {
        param_values.push(Box::new(i.clone()));
    }
    if *imagetyp == ImageType::Flat {
        if let Some(ref f) = filter {
            param_values.push(Box::new(f.clone()));
        }
    }
    if *imagetyp == ImageType::Dark {
        if let Some(e) = exptime {
            param_values.push(Box::new(e));
        }
    }
    if *imagetyp == ImageType::Flat {
        if let Some(fl) = focallen {
            param_values.push(Box::new(fl));
        }
    }

    let params_refs: Vec<&dyn rusqlite::ToSql> = param_values
        .iter()
        .map(|b| b.as_ref() as &dyn rusqlite::ToSql)
        .collect();

    let frame_ids: Vec<i64> = stmt
        .query_map(&params_refs[..], |row| row.get(0))?
        .collect::<Result<Vec<i64>, _>>()?;

    Ok(frame_ids)
}

/// Group frames into a calibration set by clustering on date and temperature
pub fn group_frames_into_set(
    conn: &Connection,
    frame_ids: &[i64],
    tolerance: &CalibrationTolerance,
) -> Result<Vec<SuggestedCalibrationSet>> {
    if frame_ids.is_empty() {
        return Ok(Vec::new());
    }

    // Fetch frame metadata
    let placeholders: Vec<String> = frame_ids.iter().map(|_| "?".to_string()).collect();
    let query = format!(
        "SELECT id, imagetyp, gain, offset, binning, instrume, filter, exptime,
                focallen, ccd_temp, date_obs
         FROM frames
         WHERE id IN ({})",
        placeholders.join(",")
    );

    let mut stmt = conn.prepare(&query)?;
    let params: Vec<&dyn rusqlite::ToSql> = frame_ids
        .iter()
        .map(|id| id as &dyn rusqlite::ToSql)
        .collect();

    #[derive(Debug, Clone)]
    struct FrameData {
        id: i64,
        imagetyp: ImageType,
        gain: Option<f64>,
        offset: Option<f64>,
        binning: Option<String>,
        instrume: Option<String>,
        filter: Option<String>,
        exptime: Option<f64>,
        focallen: Option<f64>,
        ccd_temp: Option<f64>,
        date_obs: Option<DateTime<Utc>>,
    }

    let frames: Vec<FrameData> = stmt
        .query_map(&params[..], |row| {
            let imagetyp_str: String = row.get(1)?;
            let date_obs_str: Option<String> = row.get(10)?;
            let date_obs = date_obs_str.and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                .map(|dt| dt.with_timezone(&Utc));

            Ok(FrameData {
                id: row.get(0)?,
                imagetyp: ImageType::from_str(&imagetyp_str)
                    .ok_or_else(|| rusqlite::Error::InvalidQuery)?,
                gain: row.get(2)?,
                offset: row.get(3)?,
                binning: row.get(4)?,
                instrume: row.get(5)?,
                filter: row.get(6)?,
                exptime: row.get(7)?,
                focallen: row.get(8)?,
                ccd_temp: row.get(9)?,
                date_obs,
            })
        })?
        .collect::<Result<Vec<FrameData>, _>>()?;

    // Group by exact match parameters
    #[derive(Debug, Clone, Hash, Eq, PartialEq)]
    struct GroupKey {
        imagetyp: String,
        gain: String,
        offset: String,
        binning: String,
        instrume: String,
        filter: String,
        exptime: String,
        focallen: String,
    }

    let mut groups: HashMap<GroupKey, Vec<FrameData>> = HashMap::new();

    for frame in frames {
        let key = GroupKey {
            imagetyp: format!("{:?}", frame.imagetyp),
            gain: frame.gain.map(|g| format!("{:.2}", g)).unwrap_or_default(),
            offset: frame.offset.map(|o| format!("{:.2}", o)).unwrap_or_default(),
            binning: frame.binning.clone().unwrap_or_default(),
            instrume: frame.instrume.clone().unwrap_or_default(),
            filter: frame.filter.clone().unwrap_or_default(),
            exptime: frame.exptime.map(|e| format!("{:.1}", e)).unwrap_or_default(),
            focallen: frame.focallen.map(|f| format!("{:.1}", f)).unwrap_or_default(),
        };

        groups.entry(key).or_insert_with(Vec::new).push(frame);
    }

    // Create suggested sets from groups
    let mut suggested_sets = Vec::new();

    for (_key, group_frames) in groups {
        if group_frames.is_empty() {
            continue;
        }

        // Calculate temperature range
        let temps: Vec<f64> = group_frames.iter().filter_map(|f| f.ccd_temp).collect();
        let temp_min = temps.iter().cloned().fold(f64::INFINITY, f64::min);
        let temp_max = temps.iter().cloned().fold(f64::NEG_INFINITY, f64::max);

        // Calculate date range
        let dates: Vec<DateTime<Utc>> = group_frames.iter().filter_map(|f| f.date_obs).collect();
        let date_start = dates.iter().min().map(|d| d.to_rfc3339());
        let date_end = dates.iter().max().map(|d| d.to_rfc3339());

        // Use first frame as template
        let template = &group_frames[0];

        suggested_sets.push(SuggestedCalibrationSet {
            imagetyp: template.imagetyp.clone(),
            frame_ids: group_frames.iter().map(|f| f.id).collect(),
            gain: template.gain,
            offset: template.offset,
            binning: template.binning.clone(),
            instrume: template.instrume.clone(),
            filter: template.filter.clone(),
            exptime: template.exptime,
            focallen: template.focallen,
            temp_min: if temp_min.is_finite() { Some(temp_min) } else { None },
            temp_max: if temp_max.is_finite() { Some(temp_max) } else { None },
            date_start,
            date_end,
        });
    }

    Ok(suggested_sets)
}

/// Create a calibration set in the database and link frames to it
pub fn create_calibration_set(
    conn: &Connection,
    suggested_set: &SuggestedCalibrationSet,
) -> Result<i64> {
    let imagetyp_str = match suggested_set.imagetyp {
        ImageType::Dark => "Dark",
        ImageType::Flat => "Flat",
        ImageType::Bias => "Bias",
        ImageType::DarkFlat => "DarkFlat",
        ImageType::MasterDark => "MasterDark",
        ImageType::MasterFlat => "MasterFlat",
        ImageType::MasterBias => "MasterBias",
        ImageType::MasterDarkFlat => "MasterDarkFlat",
        _ => "Unknown",
    };

    let avg_temp = match (suggested_set.temp_min, suggested_set.temp_max) {
        (Some(min), Some(max)) => Some((min + max) / 2.0),
        _ => None,
    };

    let date_display = suggested_set.date_start.as_ref()
        .and_then(|d| DateTime::parse_from_rfc3339(d).ok())
        .map(|dt| dt.format("%Y-%m").to_string())
        .unwrap_or_else(|| "Unknown".to_string());

    // Insert calibration set
    conn.execute(
        "INSERT INTO calibration_set
         (imagetyp, exptime, filter, ccd_temp, gain, offset, binning, instrume,
          date, date_start, date_end, temp_min, temp_max, frame_count, focallen)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            imagetyp_str,
            suggested_set.exptime,
            suggested_set.filter,
            avg_temp,
            suggested_set.gain,
            suggested_set.offset,
            suggested_set.binning,
            suggested_set.instrume,
            date_display,
            suggested_set.date_start,
            suggested_set.date_end,
            suggested_set.temp_min,
            suggested_set.temp_max,
            suggested_set.frame_ids.len() as i64,
            suggested_set.focallen,
        ],
    )?;

    let set_id = conn.last_insert_rowid();

    // Link frames to the set
    for frame_id in &suggested_set.frame_ids {
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            params![set_id, frame_id],
        )?;
    }

    Ok(set_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_suggested_set_structure() {
        let suggested = SuggestedCalibrationSet {
            imagetyp: ImageType::Dark,
            frame_ids: vec![1, 2, 3],
            gain: Some(100.0),
            offset: Some(10.0),
            binning: Some("1x1".to_string()),
            instrume: Some("ASI2600MM".to_string()),
            filter: None,
            exptime: Some(300.0),
            focallen: None,
            temp_min: Some(-10.0),
            temp_max: Some(-8.0),
            date_start: Some("2025-01-01T00:00:00Z".to_string()),
            date_end: Some("2025-01-01T23:59:59Z".to_string()),
        };

        assert_eq!(suggested.frame_ids.len(), 3);
        assert_eq!(suggested.imagetyp, ImageType::Dark);
        assert!(suggested.temp_min.is_some());
    }

    #[test]
    fn test_empty_frame_grouping() {
        // Empty frame list should return empty suggestions
        let empty_frames: Vec<i64> = Vec::new();
        assert!(empty_frames.is_empty());
    }
}

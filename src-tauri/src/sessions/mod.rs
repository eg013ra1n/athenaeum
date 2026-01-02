use anyhow::Result;
use chrono::{DateTime, Utc, Duration};
use std::collections::HashMap;

use crate::models::{File, Frame};

/// Detected imaging night structure
pub struct DetectedNight {
    pub start_time: String,
    pub end_time: String,
    pub sessions: Vec<DetectedSession>,
}

/// Detected session structure
pub struct DetectedSession {
    pub instrume: String,
    pub frame_ids: Vec<i64>,
    pub total_exp_time: Option<f64>,
}

/// Frame with parsed timestamp for sorting and gap detection
struct FrameWithTime {
    #[allow(dead_code)]
    file_id: i64,
    frame_id: i64,
    frame: Frame,
    timestamp: DateTime<Utc>,
}

/// Detect imaging nights and sessions from frames
///
/// Algorithm:
/// 1. Filter frames that have date_obs
/// 2. Sort all frames by date_obs
/// 3. Detect night boundaries using gap threshold
/// 4. Within each night, group by instrume to create sessions
///
/// # Arguments
/// * `frames` - List of (file_id, file, frame) tuples
/// * `gap_threshold_hours` - Time gap to detect new night
pub fn detect_sessions(
    frames: Vec<(i64, File, Frame)>,
    gap_threshold_hours: f64,
) -> Result<Vec<DetectedNight>> {
    // Filter frames with valid timestamps
    let total_frames = frames.len();
    let mut frames_with_time: Vec<FrameWithTime> = frames
        .into_iter()
        .filter_map(|(file_id, _file, frame)| {
            let frame_id = frame.id?;

            if frame.date_obs.is_none() {
                println!("Frame {} has no date_obs", frame_id);
                return None;
            }

            let timestamp = frame.date_obs?;

            Some(FrameWithTime {
                file_id,
                frame_id,
                frame,
                timestamp,
            })
        })
        .collect();

    println!("Filtered: {} frames with valid timestamps out of {} total", frames_with_time.len(), total_frames);

    if frames_with_time.is_empty() {
        println!("No frames with valid date_obs found!");
        return Ok(Vec::new());
    }

    // Sort by timestamp
    frames_with_time.sort_by_key(|f| f.timestamp);

    // Detect night boundaries using gap threshold
    let gap_duration = Duration::hours(gap_threshold_hours as i64);
    let mut nights: Vec<Vec<FrameWithTime>> = Vec::new();
    let mut current_night: Vec<FrameWithTime> = Vec::new();

    for (i, frame) in frames_with_time.into_iter().enumerate() {
        if i == 0 {
            current_night.push(frame);
        } else {
            let last_frame = current_night.last().unwrap();
            let time_diff = frame.timestamp - last_frame.timestamp;

            if time_diff > gap_duration {
                // Start new night
                if !current_night.is_empty() {
                    nights.push(current_night);
                }
                current_night = vec![frame];
            } else {
                current_night.push(frame);
            }
        }
    }

    // Don't forget the last night
    if !current_night.is_empty() {
        nights.push(current_night);
    }

    // Process each night
    let mut detected_nights = Vec::new();

    for night_frames in nights {
        if night_frames.is_empty() {
            continue;
        }

        let start_time = night_frames.first().unwrap().timestamp;
        let end_time = night_frames.last().unwrap().timestamp;

        // Group by instrume within this night
        let mut instrume_groups: HashMap<String, Vec<&FrameWithTime>> = HashMap::new();

        for frame in &night_frames {
            let instrume = frame.frame.instrume.clone().unwrap_or_else(|| "Unknown".to_string());
            instrume_groups.entry(instrume).or_default().push(frame);
        }

        // Create sessions
        let mut sessions = Vec::new();

        for (instrume, frames) in instrume_groups {
            let frame_ids: Vec<i64> = frames.iter().map(|f| f.frame_id).collect();

            // Calculate total exposure time
            let total_exp_time: Option<f64> = {
                let exptimes: Vec<f64> = frames
                    .iter()
                    .filter_map(|f| f.frame.exptime)
                    .collect();

                if exptimes.is_empty() {
                    None
                } else {
                    Some(exptimes.iter().sum())
                }
            };

            sessions.push(DetectedSession {
                instrume,
                frame_ids,
                total_exp_time,
            });
        }

        // Sort sessions by instrume name for consistency
        sessions.sort_by(|a, b| a.instrume.cmp(&b.instrume));

        detected_nights.push(DetectedNight {
            start_time: start_time.to_rfc3339(),
            end_time: end_time.to_rfc3339(),
            sessions,
        });
    }

    Ok(detected_nights)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{File, Frame, FileFormat};

    fn create_test_frame(
        id: i64,
        date_obs: &str,
        instrume: &str,
        exptime: f64,
    ) -> (i64, File, Frame) {
        let file = File {
            id: Some(id),
            path: format!("/test/{}.fits", id),
            filename: format!("test_{}.fits", id),
            size: 1000,
            modified_at: DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            format: FileFormat::FITS,
            created_at: DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            metadata_hash: None,
            content_hash: None,
        };

        let frame = Frame {
            id: Some(id),
            file_id: id,
            object: Some("M31".to_string()),
            date_obs: Some(DateTime::parse_from_rfc3339(date_obs)
                .unwrap()
                .with_timezone(&Utc)),
            telescop: None,
            instrume: Some(instrume.to_string()),
            exptime: Some(exptime),
            filter: None,
            imagetyp: None,
            is_master: false,
            gain: None,
            offset: None,
            binning: None,
            xbinning: None,
            ybinning: None,
            ccd_temp: None,
            set_temp: None,
            focallen: None,
            xpixsz: None,
            ypixsz: None,
            naxis1: None,
            naxis2: None,
            ra: None,
            dec: None,
            sitelat: None,
            lat_obs: None,
            sitelong: None,
            long_obs: None,
            objctra: None,
            objctdec: None,
            override_: false,
            swcreate: None,
            bayerpat: None,
        };

        (id, file, frame)
    }

    #[test]
    fn test_single_night_single_camera() {
        let frames = vec![
            create_test_frame(1, "2024-01-15T19:30:00Z", "ZWO ASI2600MM", 300.0),
            create_test_frame(2, "2024-01-15T19:40:00Z", "ZWO ASI2600MM", 300.0),
            create_test_frame(3, "2024-01-16T02:00:00Z", "ZWO ASI2600MM", 300.0),
        ];

        let nights = detect_sessions(frames, 6.0).unwrap();

        assert_eq!(nights.len(), 1);
        assert_eq!(nights[0].sessions.len(), 1);
        assert_eq!(nights[0].sessions[0].frame_ids.len(), 3);
        assert_eq!(nights[0].sessions[0].instrume, "ZWO ASI2600MM");
        assert_eq!(nights[0].sessions[0].total_exp_time, Some(900.0));
    }

    #[test]
    fn test_single_night_multiple_cameras() {
        let frames = vec![
            create_test_frame(1, "2024-01-15T19:30:00Z", "ZWO ASI2600MM", 300.0),
            create_test_frame(2, "2024-01-15T19:35:00Z", "ZWO ASI2600MM", 300.0),
            create_test_frame(3, "2024-01-15T19:40:00Z", "Canon EOS Ra", 180.0),
            create_test_frame(4, "2024-01-16T02:00:00Z", "ZWO ASI2600MM", 300.0),
            create_test_frame(5, "2024-01-16T02:05:00Z", "Canon EOS Ra", 180.0),
        ];

        let nights = detect_sessions(frames, 6.0).unwrap();

        assert_eq!(nights.len(), 1);
        assert_eq!(nights[0].sessions.len(), 2);

        // Sessions are sorted by instrume name
        let canon_session = nights[0].sessions.iter().find(|s| s.instrume == "Canon EOS Ra").unwrap();
        let zwo_session = nights[0].sessions.iter().find(|s| s.instrume == "ZWO ASI2600MM").unwrap();

        assert_eq!(canon_session.frame_ids.len(), 2);
        assert_eq!(canon_session.total_exp_time, Some(360.0));

        assert_eq!(zwo_session.frame_ids.len(), 3);
        assert_eq!(zwo_session.total_exp_time, Some(900.0));
    }

    #[test]
    fn test_multiple_nights() {
        let frames = vec![
            // Night 1
            create_test_frame(1, "2024-01-15T19:30:00Z", "ZWO ASI2600MM", 300.0),
            create_test_frame(2, "2024-01-16T02:00:00Z", "ZWO ASI2600MM", 300.0),
            // 8 hour gap
            // Night 2
            create_test_frame(3, "2024-01-16T18:00:00Z", "ZWO ASI2600MM", 300.0),
            create_test_frame(4, "2024-01-16T22:00:00Z", "ZWO ASI2600MM", 300.0),
        ];

        let nights = detect_sessions(frames, 6.0).unwrap();

        assert_eq!(nights.len(), 2);
        assert_eq!(nights[0].sessions[0].frame_ids.len(), 2);
        assert_eq!(nights[1].sessions[0].frame_ids.len(), 2);
    }
}

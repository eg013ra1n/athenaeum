use anyhow::{Context, Result};
use rusqlite::Connection;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};

use crate::models::ImagingNight;

/// Check if two imaging nights match (same calendar date and overlapping time ranges)
pub fn nights_match(night_a: &ImagingNight, night_b: &ImagingNight) -> Result<bool> {
    // Parse start and end times
    let start_a = DateTime::parse_from_rfc3339(&night_a.start_time)
        .context("Failed to parse night_a start_time")?
        .with_timezone(&Utc);
    let end_a = DateTime::parse_from_rfc3339(&night_a.end_time)
        .context("Failed to parse night_a end_time")?
        .with_timezone(&Utc);

    let start_b = DateTime::parse_from_rfc3339(&night_b.start_time)
        .context("Failed to parse night_b start_time")?
        .with_timezone(&Utc);
    let end_b = DateTime::parse_from_rfc3339(&night_b.end_time)
        .context("Failed to parse night_b end_time")?
        .with_timezone(&Utc);

    // Extract calendar dates (ignoring time component initially)
    let date_a_start = start_a.date_naive();
    let date_a_end = end_a.date_naive();
    let date_b_start = start_b.date_naive();
    let date_b_end = end_b.date_naive();

    // Check if they share any calendar date
    // This handles cases where observations span midnight
    let dates_a: Vec<NaiveDate> = if date_a_start == date_a_end {
        vec![date_a_start]
    } else {
        // Span multiple days
        let mut dates = vec![date_a_start];
        let mut current = date_a_start;
        while current < date_a_end {
            current = current.succ_opt().unwrap();
            dates.push(current);
        }
        dates
    };

    let dates_b: Vec<NaiveDate> = if date_b_start == date_b_end {
        vec![date_b_start]
    } else {
        // Span multiple days
        let mut dates = vec![date_b_start];
        let mut current = date_b_start;
        while current < date_b_end {
            current = current.succ_opt().unwrap();
            dates.push(current);
        }
        dates
    };

    // Check if they share any calendar date
    let same_calendar_night = dates_a.iter().any(|date_a| dates_b.contains(date_a));

    if !same_calendar_night {
        return Ok(false);
    }

    // Check if time ranges overlap
    // Two ranges [start_a, end_a] and [start_b, end_b] overlap if:
    // start_a < end_b AND start_b < end_a
    let time_ranges_overlap = start_a < end_b && start_b < end_a;

    Ok(time_ranges_overlap)
}

/// Find matching night in target frame set
pub fn find_matching_night(
    source_night: &ImagingNight,
    target_nights: &[ImagingNight],
) -> Result<Option<i64>> {
    for target_night in target_nights {
        if nights_match(source_night, target_night)? {
            return Ok(target_night.id);
        }
    }
    Ok(None)
}

/// Calculate the union of two time ranges (earliest start to latest end)
pub fn calculate_time_range_union(
    time_a_start: &str,
    time_a_end: &str,
    time_b_start: &str,
    time_b_end: &str,
) -> Result<(String, String)> {
    let start_a = DateTime::parse_from_rfc3339(time_a_start)
        .context("Failed to parse time_a_start")?
        .with_timezone(&Utc);
    let end_a = DateTime::parse_from_rfc3339(time_a_end)
        .context("Failed to parse time_a_end")?
        .with_timezone(&Utc);

    let start_b = DateTime::parse_from_rfc3339(time_b_start)
        .context("Failed to parse time_b_start")?
        .with_timezone(&Utc);
    let end_b = DateTime::parse_from_rfc3339(time_b_end)
        .context("Failed to parse time_b_end")?
        .with_timezone(&Utc);

    // Find earliest start and latest end
    let union_start = if start_a < start_b { start_a } else { start_b };
    let union_end = if end_a > end_b { end_a } else { end_b };

    Ok((
        union_start.to_rfc3339(),
        union_end.to_rfc3339(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn test_nights_match_same_night() {
        let night_a = ImagingNight {
            id: Some(1),
            frames_set_id: 1,
            start_time: "2024-10-25T01:00:00Z".to_string(),
            end_time: "2024-10-25T03:33:00Z".to_string(),
            created_at: None,
        };

        let night_b = ImagingNight {
            id: Some(2),
            frames_set_id: 2,
            start_time: "2024-10-24T19:34:00Z".to_string(), // Evening of Oct 24 (UTC)
            end_time: "2024-10-25T04:44:00Z".to_string(),   // Morning of Oct 25 (UTC)
            created_at: None,
        };

        // They share Oct 25 and time ranges overlap
        assert!(nights_match(&night_a, &night_b).unwrap());
    }

    #[test]
    fn test_nights_no_match_different_dates() {
        let night_a = ImagingNight {
            id: Some(1),
            frames_set_id: 1,
            start_time: "2024-10-25T01:00:00Z".to_string(),
            end_time: "2024-10-25T03:00:00Z".to_string(),
            created_at: None,
        };

        let night_b = ImagingNight {
            id: Some(2),
            frames_set_id: 2,
            start_time: "2024-10-26T01:00:00Z".to_string(),
            end_time: "2024-10-26T03:00:00Z".to_string(),
            created_at: None,
        };

        // Different dates, no overlap
        assert!(!nights_match(&night_a, &night_b).unwrap());
    }

    #[test]
    fn test_nights_no_match_same_date_no_overlap() {
        let night_a = ImagingNight {
            id: Some(1),
            frames_set_id: 1,
            start_time: "2024-10-25T01:00:00Z".to_string(),
            end_time: "2024-10-25T03:00:00Z".to_string(),
            created_at: None,
        };

        let night_b = ImagingNight {
            id: Some(2),
            frames_set_id: 2,
            start_time: "2024-10-25T05:00:00Z".to_string(),
            end_time: "2024-10-25T07:00:00Z".to_string(),
            created_at: None,
        };

        // Same date but time ranges don't overlap
        assert!(!nights_match(&night_a, &night_b).unwrap());
    }

    #[test]
    fn test_calculate_time_range_union() {
        let (start, end) = calculate_time_range_union(
            "2024-10-25T01:00:00Z",
            "2024-10-25T03:00:00Z",
            "2024-10-25T02:00:00Z",
            "2024-10-25T05:00:00Z",
        ).unwrap();

        assert_eq!(start, "2024-10-25T01:00:00+00:00");
        assert_eq!(end, "2024-10-25T05:00:00+00:00");
    }
}

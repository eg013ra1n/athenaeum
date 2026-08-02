use anyhow::Result;
use rusqlite::Connection;
use chrono::{DateTime, Utc};

use crate::coordinates::{spherical_mean, format_ra_sexagesimal, format_dec_sexagesimal};
use crate::models::Frame;

/// Calculated metadata for a frame set
#[derive(Debug, Clone)]
pub struct FrameSetMetadata {
    pub date_obs_start: Option<String>,
    pub date_obs_end: Option<String>,
    pub objctra: Option<String>,
    pub objctdec: Option<String>,
    pub total_exp_time: Option<f64>,
    pub avg_rotation: Option<f64>,
    pub min_rotation: Option<f64>,
    pub max_rotation: Option<f64>,
}

/// Calculate frame set metadata from a list of frame IDs
pub fn calculate_metadata_from_frame_ids(
    frame_ids: &[i64],
    conn: &Connection,
) -> Result<FrameSetMetadata> {
    if frame_ids.is_empty() {
        return Ok(FrameSetMetadata {
            date_obs_start: None,
            date_obs_end: None,
            objctra: None,
            objctdec: None,
            total_exp_time: None,
            avg_rotation: None,
            min_rotation: None,
            max_rotation: None,
        });
    }

    // Build the query to get all frames
    let placeholders = frame_ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let query = format!(
        "SELECT fr.id, fr.file_id, fr.object, fr.date_obs, fr.telescop, fr.instrume,
                fr.exptime, fr.filter, fr.imagetyp, fr.is_master, fr.gain, fr.offset, fr.binning,
                fr.xbinning, fr.ybinning, fr.ccd_temp, fr.set_temp, fr.focallen,
                fr.xpixsz, fr.ypixsz, fr.naxis1, fr.naxis2, fr.ra, fr.dec, fr.sitelat, fr.lat_obs,
                fr.sitelong, fr.long_obs, fr.objctra, fr.objctdec, fr.override, fr.rotation,
                fr.uuid, fr.updated_at
         FROM frames fr
         WHERE fr.id IN ({})",
        placeholders
    );

    let mut stmt = conn.prepare(&query)?;
    let params: Vec<&dyn rusqlite::ToSql> = frame_ids
        .iter()
        .map(|id| id as &dyn rusqlite::ToSql)
        .collect();

    let frames: Vec<Frame> = stmt
        .query_map(&params[..], |row| {
            let date_obs_str: Option<String> = row.get(3)?;
            let date_obs = date_obs_str.and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                .map(|dt| dt.with_timezone(&Utc));

            let imagetyp_str: Option<String> = row.get(8)?;
            let imagetyp = imagetyp_str.and_then(|s| crate::models::ImageType::from_str(&s));

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
                is_master: row.get::<_, i32>(9)? == 1,
                gain: row.get(10)?,
                offset: row.get(11)?,
                binning: row.get(12)?,
                xbinning: row.get(13)?,
                ybinning: row.get(14)?,
                ccd_temp: row.get(15)?,
                set_temp: row.get(16)?,
                focallen: row.get(17)?,
                xpixsz: row.get(18)?,
                ypixsz: row.get(19)?,
                naxis1: row.get(20)?,
                naxis2: row.get(21)?,
                ra: row.get(22)?,
                dec: row.get(23)?,
                sitelat: row.get(24)?,
                lat_obs: row.get(25)?,
                sitelong: row.get(26)?,
                long_obs: row.get(27)?,
                objctra: row.get(28)?,
                objctdec: row.get(29)?,
                override_: row.get::<_, i32>(30)? == 1,
                swcreate: None,
                bayerpat: None,
                xbayroff: None,
                ybayroff: None,
                roworder: None,
                rotation: row.get(31)?,
                uuid: row.get(32)?,
                updated_at: row.get(33)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    calculate_metadata_from_frames(&frames)
}

/// Calculate frame set metadata from a list of frames
pub fn calculate_metadata_from_frames(frames: &[Frame]) -> Result<FrameSetMetadata> {
    if frames.is_empty() {
        return Ok(FrameSetMetadata {
            date_obs_start: None,
            date_obs_end: None,
            objctra: None,
            objctdec: None,
            total_exp_time: None,
            avg_rotation: None,
            min_rotation: None,
            max_rotation: None,
        });
    }

    // Calculate total exposure time
    let total_exp_time: f64 = frames
        .iter()
        .filter_map(|f| f.exptime)
        .sum();

    let total_exp_time = if total_exp_time > 0.0 {
        Some(total_exp_time)
    } else {
        None
    };

    // Calculate date_obs_start and date_obs_end
    let dates: Vec<DateTime<Utc>> = frames
        .iter()
        .filter_map(|f| f.date_obs)
        .collect();

    let (date_obs_start, date_obs_end) = if !dates.is_empty() {
        let earliest = dates.iter().min().unwrap();
        let latest = dates.iter().max().unwrap();
        (
            Some(earliest.to_rfc3339()),
            Some(latest.to_rfc3339()),
        )
    } else {
        (None, None)
    };

    // Calculate average coordinates using spherical mean
    let coords: Vec<(f64, f64)> = frames
        .iter()
        .filter_map(|f| {
            // Try numeric coordinates first
            if let (Some(ra), Some(dec)) = (f.ra, f.dec) {
                return Some((ra, dec));
            }
            // Try parsing sexagesimal coordinates
            if let (Some(ref ra_str), Some(ref dec_str)) = (&f.objctra, &f.objctdec) {
                use crate::coordinates::{parse_ra_sexagesimal, parse_dec_sexagesimal};
                if let (Ok(ra), Ok(dec)) = (
                    parse_ra_sexagesimal(ra_str),
                    parse_dec_sexagesimal(dec_str),
                ) {
                    return Some((ra, dec));
                }
            }
            None
        })
        .collect();

    let (objctra, objctdec) = if !coords.is_empty() {
        match spherical_mean(&coords) {
            Ok((ra_deg, dec_deg)) => {
                let ra_str = format_ra_sexagesimal(ra_deg);
                let dec_str = format_dec_sexagesimal(dec_deg);
                (Some(ra_str), Some(dec_str))
            }
            Err(_) => (None, None),
        }
    } else {
        (None, None)
    };

    // Calculate rotation stats (circular mean for avg, simple min/max for range)
    let rotations: Vec<f64> = frames
        .iter()
        .filter_map(|f| f.rotation)
        .collect();

    let (avg_rotation, min_rotation, max_rotation) = if !rotations.is_empty() {
        let min_rot = rotations.iter().cloned().fold(f64::INFINITY, f64::min);
        let max_rot = rotations.iter().cloned().fold(f64::NEG_INFINITY, f64::max);

        // Circular mean via atan2(mean_sin, mean_cos)
        let sum_sin: f64 = rotations.iter().map(|r| r.to_radians().sin()).sum();
        let sum_cos: f64 = rotations.iter().map(|r| r.to_radians().cos()).sum();
        let avg_rot = sum_sin.atan2(sum_cos).to_degrees();
        // Normalize to 0..360
        let avg_rot = if avg_rot < 0.0 { avg_rot + 360.0 } else { avg_rot };

        (Some(avg_rot), Some(min_rot), Some(max_rot))
    } else {
        (None, None, None)
    };

    Ok(FrameSetMetadata {
        date_obs_start,
        date_obs_end,
        objctra,
        objctdec,
        total_exp_time,
        avg_rotation,
        min_rotation,
        max_rotation,
    })
}

/// Calculate frame set metadata from all frames in a frame set
pub fn calculate_metadata_for_frame_set(
    frames_set_id: i64,
    conn: &Connection,
) -> Result<FrameSetMetadata> {
    // Get all frame IDs in the frame set by traversing the hierarchy
    let mut stmt = conn.prepare(
        "SELECT DISTINCT sm.frame_id
         FROM session_members sm
         JOIN sessions s ON sm.session_id = s.id
         JOIN imaging_nights in_tbl ON s.imaging_night_id = in_tbl.id
         WHERE in_tbl.frames_set_id = ?"
    )?;

    let frame_ids: Vec<i64> = stmt
        .query_map([frames_set_id], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;

    calculate_metadata_from_frame_ids(&frame_ids, conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn test_calculate_empty_metadata() {
        let frames: Vec<Frame> = vec![];
        let result = calculate_metadata_from_frames(&frames).unwrap();

        assert!(result.date_obs_start.is_none());
        assert!(result.date_obs_end.is_none());
        assert!(result.objctra.is_none());
        assert!(result.objctdec.is_none());
        assert!(result.total_exp_time.is_none());
    }

    #[test]
    fn test_calculate_metadata_single_frame() {
        let frame = Frame {
            id: Some(1),
            file_id: 1,
            object: Some("M31".to_string()),
            date_obs: Some(Utc.with_ymd_and_hms(2024, 10, 25, 20, 30, 0).unwrap()),
            exptime: Some(300.0),
            ra: Some(10.684722),
            dec: Some(41.268889),
            telescop: None,
            instrume: None,
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
            sitelat: None,
            lat_obs: None,
            sitelong: None,
            long_obs: None,
            objctra: None,
            objctdec: None,
            override_: false,
            swcreate: None,
            bayerpat: None,
            xbayroff: None,
            ybayroff: None,
            roworder: None,
            rotation: None,
            uuid: None,
            updated_at: None,
        };

        let result = calculate_metadata_from_frames(&[frame]).unwrap();

        assert!(result.date_obs_start.is_some());
        assert!(result.date_obs_end.is_some());
        assert_eq!(result.date_obs_start, result.date_obs_end);
        assert_eq!(result.total_exp_time, Some(300.0));
        assert!(result.objctra.is_some());
        assert!(result.objctdec.is_some());
    }
}

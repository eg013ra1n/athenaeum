use std::collections::HashMap;

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
    /// Lights the center and rotation were taken from ([`set_pointing`]).
    pub pointing_frames: usize,
    /// True when those lights are all plate-solved by Athenaeum.
    pub pointing_solved: bool,
}

/// What one light contributes to its set's center and rotation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FramePointing {
    pub ra: f64,
    pub dec: f64,
    /// `frames.rotation` (CROTA2 convention, see `docs/skychart/README.md`).
    pub rotation: Option<f64>,
    /// Athenaeum solved it (a `plate_solves` row exists): `ra`/`dec` are the
    /// image center and `rotation` is atan2(−CD1_2, CD2_2).
    pub solved: bool,
    pub in_black_hole: bool,
    /// Field of view (width, height) in degrees: from the solve's CD matrix
    /// when solved, else from the header (`api::spatial::frame_fov`).
    pub fov: Option<(f64, f64)>,
}

/// A set's center and rotation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SetPointing {
    pub ra: f64,
    pub dec: f64,
    pub avg_rotation: Option<f64>,
    pub min_rotation: Option<f64>,
    pub max_rotation: Option<f64>,
    pub frames: usize,
    pub solved: bool,
    /// Median field of view (width, height) in degrees over the same lights.
    pub fov: Option<(f64, f64)>,
}

/// The one rule for a set's center and rotation, shared by the stored
/// frame-set metadata and the Sky Chart:
/// - lights in the Black Hole are rejected and left out (all of them are used
///   only when every light is in the Black Hole);
/// - when any remaining light is plate-solved by Athenaeum, only the solved
///   ones count: an unsolved light carries the mount's or the target's
///   coordinates from its header, and its rotation may come from another
///   program in another sign convention (a header 76.1° beside a solve's
///   283.5° for the same framing averages to ~0°);
/// - the center is the spherical mean (no RA wrap at 0h), the rotation the
///   circular mean, and min / max are the extreme circular deviations from it;
/// - the field of view is the median width and height over the same lights
///   (a solved light's comes from its CD matrix, not FOCALLEN / XPIXSZ).
pub fn set_pointing(frames: &[FramePointing]) -> Option<SetPointing> {
    let kept: Vec<&FramePointing> = frames.iter().filter(|f| !f.in_black_hole).collect();
    let kept: Vec<&FramePointing> = if kept.is_empty() { frames.iter().collect() } else { kept };
    let solved: Vec<&FramePointing> = kept.iter().copied().filter(|f| f.solved).collect();
    let (used, solved_used) = if solved.is_empty() { (kept, false) } else { (solved, true) };
    let coords: Vec<(f64, f64)> = used.iter().map(|f| (f.ra, f.dec)).collect();
    let (ra, dec) = spherical_mean(&coords).ok()?;
    let rotations: Vec<f64> = used.iter().filter_map(|f| f.rotation).collect();
    let (avg_rotation, min_rotation, max_rotation) = match rotation_stats(&rotations) {
        Some((a, lo, hi)) => (Some(a), Some(lo), Some(hi)),
        None => (None, None, None),
    };
    let fovs: Vec<(f64, f64)> = used.iter().filter_map(|f| f.fov).collect();
    let fov = median(fovs.iter().map(|f| f.0).collect()).zip(median(fovs.iter().map(|f| f.1).collect()));
    Some(SetPointing { ra, dec, avg_rotation, min_rotation, max_rotation, frames: used.len(), solved: solved_used, fov })
}

fn median(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    let mid = values.len() / 2;
    Some(if values.len() % 2 == 0 { (values[mid - 1] + values[mid]) / 2.0 } else { values[mid] })
}

fn norm360(deg: f64) -> f64 {
    deg.rem_euclid(360.0)
}

/// Circular mean, and the smallest / largest circular deviation from it, of
/// angles in degrees; all three in [0, 360). `None` for no angles or angles
/// that cancel out.
pub fn rotation_stats(rotations: &[f64]) -> Option<(f64, f64, f64)> {
    if rotations.is_empty() {
        return None;
    }
    let sum_sin: f64 = rotations.iter().map(|r| r.to_radians().sin()).sum();
    let sum_cos: f64 = rotations.iter().map(|r| r.to_radians().cos()).sum();
    if sum_sin.hypot(sum_cos) < 1e-9 * rotations.len() as f64 {
        return None;
    }
    let mean = norm360(sum_sin.atan2(sum_cos).to_degrees());
    let deviation = |r: f64| norm360(r - mean + 180.0) - 180.0;
    let lo = rotations.iter().map(|&r| deviation(r)).fold(f64::INFINITY, f64::min);
    let hi = rotations.iter().map(|&r| deviation(r)).fold(f64::NEG_INFINITY, f64::max);
    Some((mean, norm360(mean + lo), norm360(mean + hi)))
}

/// A frame's coordinates: the numeric RA/Dec, else its sexagesimal header pair.
fn frame_coords(f: &Frame) -> Option<(f64, f64)> {
    if let (Some(ra), Some(dec)) = (f.ra, f.dec) {
        return Some((ra, dec));
    }
    if let (Some(ref ra_str), Some(ref dec_str)) = (&f.objctra, &f.objctdec) {
        use crate::coordinates::{parse_ra_sexagesimal, parse_dec_sexagesimal};
        if let (Ok(ra), Ok(dec)) = (parse_ra_sexagesimal(ra_str), parse_dec_sexagesimal(dec_str)) {
            return Some((ra, dec));
        }
    }
    None
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
            pointing_frames: 0,
            pointing_solved: false,
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

    // Which lights are solved by Athenaeum and which are in the Black Hole.
    let flag_query = format!(
        "SELECT fr.id,
                EXISTS (SELECT 1 FROM plate_solves ps WHERE ps.frame_id = fr.id),
                EXISTS (SELECT 1 FROM black_hole bh WHERE bh.file_id = fr.file_id)
         FROM frames fr WHERE fr.id IN ({})",
        placeholders
    );
    let mut flag_stmt = conn.prepare(&flag_query)?;
    let flags: HashMap<i64, (bool, bool)> = flag_stmt
        .query_map(&params[..], |row| Ok((row.get(0)?, (row.get(1)?, row.get(2)?))))?
        .collect::<Result<_, _>>()?;

    metadata_from_frames(&frames, &flags)
}

/// Calculate frame set metadata from a list of frames. With no solve or
/// Black Hole information every frame counts as an unsolved, kept light.
pub fn calculate_metadata_from_frames(frames: &[Frame]) -> Result<FrameSetMetadata> {
    metadata_from_frames(frames, &HashMap::new())
}

/// `flags`: frame id → (solved by Athenaeum, in the Black Hole).
fn metadata_from_frames(frames: &[Frame], flags: &HashMap<i64, (bool, bool)>) -> Result<FrameSetMetadata> {
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
            pointing_frames: 0,
            pointing_solved: false,
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

    // Center and rotation: the one rule (`set_pointing`).
    let pointings: Vec<FramePointing> = frames
        .iter()
        .filter_map(|f| {
            let (ra, dec) = frame_coords(f)?;
            let (solved, in_black_hole) = f.id.and_then(|id| flags.get(&id).copied()).unwrap_or((false, false));
            Some(FramePointing { ra, dec, rotation: f.rotation, solved, in_black_hole, fov: None })
        })
        .collect();
    let pointing = set_pointing(&pointings);
    let (objctra, objctdec) = match pointing {
        Some(p) => (Some(format_ra_sexagesimal(p.ra)), Some(format_dec_sexagesimal(p.dec))),
        None => (None, None),
    };
    let (avg_rotation, min_rotation, max_rotation) = match pointing {
        Some(p) => (p.avg_rotation, p.min_rotation, p.max_rotation),
        // No coordinates at all: rotation still from every frame that has one.
        None => match rotation_stats(&frames.iter().filter_map(|f| f.rotation).collect::<Vec<_>>()) {
            Some((a, lo, hi)) => (Some(a), Some(lo), Some(hi)),
            None => (None, None, None),
        },
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
        pointing_frames: pointing.map_or(0, |p| p.frames),
        pointing_solved: pointing.is_some_and(|p| p.solved),
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

    fn fp(ra: f64, dec: f64, rotation: Option<f64>, solved: bool, in_black_hole: bool) -> FramePointing {
        FramePointing { ra, dec, rotation, solved, in_black_hole, fov: None }
    }

    #[test]
    fn solved_lights_win_over_header_coordinates() {
        // Two solved lights near M 31 and an unsolved one with the mount's
        // coordinates half a degree off and a header rotation of the other sign.
        let p = set_pointing(&[
            fp(10.67, 41.10, Some(283.5), true, false),
            fp(10.68, 41.11, Some(283.4), true, false),
            fp(11.20, 40.60, Some(76.1), false, false),
        ])
        .unwrap();
        assert!(p.solved);
        assert_eq!(p.frames, 2);
        assert!((p.ra - 10.675).abs() < 1e-3 && (p.dec - 41.105).abs() < 1e-3);
        assert!((p.avg_rotation.unwrap() - 283.45).abs() < 1e-6);
    }

    #[test]
    fn black_hole_lights_are_left_out_unless_all_are_there() {
        let p = set_pointing(&[fp(10.0, 0.0, None, true, false), fp(50.0, 0.0, None, true, true)]).unwrap();
        assert!((p.ra - 10.0).abs() < 1e-9 && p.frames == 1);
        let all = set_pointing(&[fp(10.0, 0.0, None, false, true), fp(12.0, 0.0, None, false, true)]).unwrap();
        assert_eq!(all.frames, 2);
        assert!((all.ra - 11.0).abs() < 1e-6);
    }

    #[test]
    fn fov_is_the_median_over_the_pointing_lights() {
        let with = |fov: (f64, f64), solved: bool, bh: bool| FramePointing { fov: Some(fov), ..fp(10.0, 41.0, None, solved, bh) };
        // An unsolved light with a wrong header FOCALLEN, a Black Hole light from
        // another rig, and three solved lights (one binned 2×2 by mistake).
        let p = set_pointing(&[
            with((1.2, 0.8), false, false),
            with((0.5, 0.3), true, true),
            with((3.10, 2.07), true, false),
            with((3.11, 2.08), true, false),
            with((1.55, 1.04), true, false),
        ])
        .unwrap();
        assert_eq!(p.fov, Some((3.10, 2.07)));
        let even = set_pointing(&[with((2.0, 1.0), true, false), with((3.0, 2.0), true, false)]).unwrap();
        assert_eq!(even.fov, Some((2.5, 1.5)));
        assert_eq!(set_pointing(&[fp(1.0, 1.0, None, true, false)]).unwrap().fov, None);
    }

    #[test]
    fn unsolved_sets_still_get_a_center() {
        let p = set_pointing(&[fp(83.8, -5.4, Some(10.0), false, false)]).unwrap();
        assert!(!p.solved);
        assert_eq!(p.frames, 1);
    }

    #[test]
    fn the_center_does_not_wrap_at_0h() {
        let p = set_pointing(&[fp(359.9, 20.0, None, true, false), fp(0.1, 20.0, None, true, false)]).unwrap();
        assert!(p.ra < 0.01 || p.ra > 359.99, "{}", p.ra);
    }

    #[test]
    fn rotation_range_is_circular() {
        let (avg, lo, hi) = rotation_stats(&[359.0, 1.0, 0.5]).unwrap();
        assert!(avg < 0.5 || avg > 359.9, "{avg}");
        assert!((lo - 359.0).abs() < 1e-9 && (hi - 1.0).abs() < 1e-9, "{lo} {hi}");
        let (_, lo, hi) = rotation_stats(&[283.5, 283.4, 283.6]).unwrap();
        assert!((lo - 283.4).abs() < 1e-9 && (hi - 283.6).abs() < 1e-9);
        assert!(rotation_stats(&[]).is_none());
        assert!(rotation_stats(&[0.0, 180.0]).is_none(), "opposite angles have no mean");
    }
}


use chrono::Datelike;
use rusqlite::Connection;

use astroimage::platesolving::SolveHints;

use crate::coordinates::{parse_dec_sexagesimal, parse_ra_sexagesimal};
use crate::models::Frame;

/// Extract plate-solving hints from a frame's metadata.
///
/// Priority for coordinates:
/// 1. Direct numeric ra/dec
/// 2. Sexagesimal objctra/objctdec
/// 3. Nearby solved frame in the same directory
///
/// FOV and pixel scale always come from focallen + xpixsz + naxis1.
pub fn extract_hints(frame: &Frame, conn: Option<&Connection>) -> SolveHints {
    let mut hints = SolveHints::default();

    // Try numeric RA/Dec first
    if let (Some(ra), Some(dec)) = (frame.ra, frame.dec) {
        hints.ra = Some(ra);
        hints.dec = Some(dec);
    }

    // Fall back to sexagesimal strings
    if hints.ra.is_none() || hints.dec.is_none() {
        if let (Some(ref ra_str), Some(ref dec_str)) = (&frame.objctra, &frame.objctdec) {
            if let (Ok(ra), Ok(dec)) = (parse_ra_sexagesimal(ra_str), parse_dec_sexagesimal(dec_str))
            {
                hints.ra = Some(ra);
                hints.dec = Some(dec);
            }
        }
    }

    // Fall back to nearby solved frame
    if hints.ra.is_none() && conn.is_some() {
        if let Some((ra, dec)) = find_nearby_solved_frame(frame, conn.unwrap()) {
            hints.ra = Some(ra);
            hints.dec = Some(dec);
        }
    }

    // Compute pixel scale and FOV from optics
    if let (Some(focallen), Some(xpixsz)) = (frame.focallen, frame.xpixsz) {
        if focallen > 0.0 && xpixsz > 0.0 {
            let pixel_size_mm = xpixsz / 1000.0;
            let binning = frame.xbinning.unwrap_or(1).max(1) as f64;
            let effective_pixel_mm = pixel_size_mm * binning;
            let arcsec_per_px = (effective_pixel_mm / focallen).atan().to_degrees() * 3600.0;
            hints.pixel_scale_arcsec = Some(arcsec_per_px);

            if let Some(naxis1) = frame.naxis1 {
                // FOV = 2 * atan(sensor_size / (2 * focal_length))
                let sensor_mm = naxis1 as f64 * effective_pixel_mm;
                let fov_deg = 2.0 * (sensor_mm / (2.0 * focallen)).atan().to_degrees();
                hints.fov_deg = Some(fov_deg);
            }
        }
    }

    hints.rotation = frame.rotation;

    hints
}

/// Look for a recently solved frame in the same directory.
fn find_nearby_solved_frame(frame: &Frame, conn: &Connection) -> Option<(f64, f64)> {
    // Find frames from the same file directory that have been plate-solved
    let result: Result<(f64, f64), _> = conn.query_row(
        "SELECT ps.crval1, ps.crval2
         FROM plate_solves ps
         JOIN frames f ON f.id = ps.frame_id
         JOIN files fl ON fl.id = f.file_id
         JOIN files fl2 ON fl2.id = ?1
         WHERE fl.id != fl2.id
           AND substr(fl.path, 1, length(fl.path) - length(fl.filename))
             = substr(fl2.path, 1, length(fl2.path) - length(fl2.filename))
         ORDER BY ps.solved_at DESC
         LIMIT 1",
        [frame.file_id],
        |row| Ok((row.get::<_, f64>(0)?, row.get::<_, f64>(1)?)),
    );

    result.ok()
}

/// Extract the observation epoch as a Julian year from the frame's date_obs.
/// Falls back to 2025.0 if no date is available.
pub fn observation_epoch(frame: &Frame) -> f64 {
    match &frame.date_obs {
        Some(dt) => {
            let year = dt.format("%Y").to_string().parse::<f64>().unwrap_or(2025.0);
            let day_of_year = dt.ordinal() as f64;
            let days_in_year = if dt.format("%Y").to_string().parse::<i32>().unwrap_or(2025) % 4
                == 0
            {
                366.0
            } else {
                365.0
            };
            year + day_of_year / days_in_year
        }
        None => 2025.0,
    }
}

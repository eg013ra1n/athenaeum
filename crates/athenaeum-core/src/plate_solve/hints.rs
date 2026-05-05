use chrono::Datelike;
use rusqlite::Connection;

use astroimage::platesolving::SolveHints;

use crate::coordinates::{parse_dec_sexagesimal, parse_ra_sexagesimal};
use crate::models::Frame;

/// Extract plate-solving hints from a frame's metadata.
///
/// Priority for coordinates:
/// 1. Sexagesimal objctra/objctdec — the *planned* target the user/sequencer
///    aimed at. Reliable even when the mount mis-syncs.
/// 2. Direct numeric ra/dec — the mount's *reported* pointing. Wrong on
///    mis-synced mounts (e.g., dual-OTA setups where one NINA instance had
///    a stale sync at the time the FITS was written).
/// 3. Nearby solved frame in the same directory.
///
/// In all three branches, sentinel values (NULL, exact 0/0, or sexagesimal
/// "00 00 00" / "+00 00 00" / "00:00:00") are rejected — they're FITS-pipeline
/// placeholders, not actual sky positions.
///
/// FOV and pixel scale always come from focallen + xpixsz + naxis1.
pub fn extract_hints(frame: &Frame, conn: Option<&Connection>) -> SolveHints {
    let mut hints = SolveHints::default();

    // Try sexagesimal OBJCTRA/OBJCTDEC first — reflects user intent, immune
    // to mount-sync drift.
    if let (Some(ref ra_str), Some(ref dec_str)) = (&frame.objctra, &frame.objctdec) {
        if let (Ok(ra), Ok(dec)) = (parse_ra_sexagesimal(ra_str), parse_dec_sexagesimal(dec_str)) {
            if !is_sentinel_position(ra, dec) {
                hints.ra = Some(ra);
                hints.dec = Some(dec);
            }
        }
    }

    // Fall back to numeric RA/Dec from the mount.
    if hints.ra.is_none() || hints.dec.is_none() {
        if let (Some(ra), Some(dec)) = (frame.ra, frame.dec) {
            if !is_sentinel_position(ra, dec) {
                hints.ra = Some(ra);
                hints.dec = Some(dec);
            }
        }
    }

    // Fall back to nearby solved frame.
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

/// `(0, 0)` is the FITS-pipeline placeholder for "RA/Dec not actually set"
/// — nobody legitimately images at the celestial-equator vernal-point. Any
/// hint that lands within 1e-6° of (0, 0) is treated as a sentinel and
/// rejected so it doesn't poison the positional-prior gate downstream.
fn is_sentinel_position(ra: f64, dec: f64) -> bool {
    ra.abs() < 1e-6 && dec.abs() < 1e-6
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

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_with(
        ra: Option<f64>,
        dec: Option<f64>,
        objctra: Option<&str>,
        objctdec: Option<&str>,
    ) -> Frame {
        Frame {
            ra,
            dec,
            objctra: objctra.map(str::to_string),
            objctdec: objctdec.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn sentinel_zero_zero_is_rejected() {
        assert!(is_sentinel_position(0.0, 0.0));
        assert!(is_sentinel_position(1e-9, -1e-9));
        assert!(!is_sentinel_position(0.65, 67.28));
        assert!(!is_sentinel_position(0.0001, 0.0001));
    }

    #[test]
    fn sexagesimal_takes_precedence_over_numeric_when_mount_misreports() {
        // Real-world failure mode: 94edph + ASI2600MC FITS file. Mount sync was
        // wrong, so RA/DEC=(91.96, +69.78) but OBJCTRA/OBJCTDEC=(LDN 1272 at
        // ~0.65°, +67.28°). The sexagesimal target reflects user intent and
        // matches the partner mono scope's actual solve.
        let frame = frame_with(
            Some(91.9641735356973),
            Some(69.77953125),
            Some("00 02 36"),
            Some("+67 16 59"),
        );
        let hints = extract_hints(&frame, None);
        let ra = hints.ra.expect("hint RA must be set");
        let dec = hints.dec.expect("hint DEC must be set");
        // Should be the LDN 1272 sexagesimal (~0.65°, +67.28°), not the
        // mount-reported numeric (91.96°, +69.78°).
        assert!(
            (ra - 0.65).abs() < 0.05,
            "expected hint RA ~0.65° from OBJCTRA, got {ra}"
        );
        assert!(
            (dec - 67.28).abs() < 0.05,
            "expected hint DEC ~+67.28° from OBJCTDEC, got {dec}"
        );
    }

    #[test]
    fn sexagesimal_sentinel_falls_through_to_numeric() {
        // ATR2600M FITS files write OBJCTRA="00 00 00" / OBJCTDEC="+00 00 00"
        // as a placeholder. We must NOT use that — fall through to numeric,
        // and if numeric is set & non-zero, use it.
        let frame = frame_with(
            Some(0.642),
            Some(67.297),
            Some("00 00 00"),
            Some("+00 00 00"),
        );
        let hints = extract_hints(&frame, None);
        assert_eq!(hints.ra, Some(0.642));
        assert_eq!(hints.dec, Some(67.297));
    }

    #[test]
    fn both_sentinels_returns_no_position_hint() {
        // Both sources are sentinels — the hint must be None (true blind solve).
        let frame = frame_with(Some(0.0), Some(0.0), Some("00 00 00"), Some("+00 00 00"));
        let hints = extract_hints(&frame, None);
        assert_eq!(hints.ra, None);
        assert_eq!(hints.dec, None);
    }

    #[test]
    fn no_coords_anywhere_returns_no_position_hint() {
        let frame = frame_with(None, None, None, None);
        let hints = extract_hints(&frame, None);
        assert_eq!(hints.ra, None);
        assert_eq!(hints.dec, None);
    }

    #[test]
    fn numeric_only_is_used_when_no_objctra() {
        let frame = frame_with(Some(123.45), Some(-45.67), None, None);
        let hints = extract_hints(&frame, None);
        assert_eq!(hints.ra, Some(123.45));
        assert_eq!(hints.dec, Some(-45.67));
    }

    #[test]
    fn numeric_zero_zero_sentinel_rejected_with_no_sexagesimal() {
        let frame = frame_with(Some(0.0), Some(0.0), None, None);
        let hints = extract_hints(&frame, None);
        assert_eq!(hints.ra, None);
        assert_eq!(hints.dec, None);
    }
}

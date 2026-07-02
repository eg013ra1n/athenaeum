use chrono::Datelike;
use rusqlite::Connection;
use serde::Serialize;

use astroimage::platesolving::SolveHints;

use crate::coordinates::{parse_dec_sexagesimal, parse_ra_sexagesimal};
use crate::models::Frame;

/// Field of view (degrees) from optics. `XPIXSZ` is the effective saved-pixel
/// pitch (binning already folded in), so it is used directly — never multiplied
/// by binning. `FOV = 2·atan(sensor_mm / (2·focallen))`, sensor_mm = naxis1·(xpixsz/1000).
pub fn fov_from_optics(
    focallen: Option<f64>,
    xpixsz: Option<f64>,
    naxis1: Option<i32>,
) -> Option<f64> {
    let (focallen, xpixsz, naxis1) = (focallen?, xpixsz?, naxis1?);
    if focallen <= 0.0 || xpixsz <= 0.0 || naxis1 <= 0 {
        return None;
    }
    let pixel_size_mm = xpixsz / 1000.0;
    let sensor_mm = naxis1 as f64 * pixel_size_mm;
    Some(2.0 * (sensor_mm / (2.0 * focallen)).atan().to_degrees())
}

/// FOV (degrees) of a frame from its optics, or `None` if optics are missing.
pub fn frame_fov_deg(frame: &Frame) -> Option<f64> {
    fov_from_optics(frame.focallen, frame.xpixsz, frame.naxis1)
}

/// Field-of-view summary across a set of LIGHT frames (the catalog-tier
/// recommendation input).
#[derive(Clone, Debug, Serialize)]
pub struct FovSummary {
    pub light_count: u32,
    pub computable_count: u32,
    pub min_fov_deg: Option<f64>,
    pub narrowest_instrume: Option<String>,
}

/// Aggregate `(focallen, xpixsz, naxis1, instrume)` rows into an `FovSummary`,
/// keeping the globally narrowest computable field.
pub fn fov_summary<I>(rows: I) -> FovSummary
where
    I: IntoIterator<Item = (Option<f64>, Option<f64>, Option<i32>, Option<String>)>,
{
    let mut light_count = 0u32;
    let mut computable_count = 0u32;
    let mut min_fov_deg: Option<f64> = None;
    let mut narrowest_instrume: Option<String> = None;
    for (focallen, xpixsz, naxis1, instrume) in rows {
        light_count += 1;
        if let Some(fov) = fov_from_optics(focallen, xpixsz, naxis1) {
            computable_count += 1;
            if min_fov_deg.map_or(true, |m| fov < m) {
                min_fov_deg = Some(fov);
                narrowest_instrume = instrume;
            }
        }
    }
    FovSummary { light_count, computable_count, min_fov_deg, narrowest_instrume }
}

/// Query LIGHT frames and summarise their fields of view.
pub fn frame_fov_summary(conn: &Connection) -> rusqlite::Result<FovSummary> {
    let mut stmt = conn.prepare(
        "SELECT focallen, xpixsz, naxis1, instrume FROM frames WHERE imagetyp LIKE 'LIGHT%'",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, Option<f64>>(0)?,
                r.get::<_, Option<f64>>(1)?,
                r.get::<_, Option<i32>>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(fov_summary(rows))
}

/// Extract plate-solving hints from a frame's metadata.
///
/// Priority for coordinates:
/// 1. Sexagesimal objctra/objctdec — the *planned* target the user/sequencer
///    aimed at. Reliable even when the mount mis-syncs.
/// 2. Direct numeric ra/dec — the mount's *reported* pointing. Wrong on
///    mis-synced mounts (e.g., dual-OTA setups where one NINA instance had
///    a stale sync at the time the FITS was written).
/// 3. Stored FITS-header WCS (`CRVAL1`/`CRVAL2`) — the file's own embedded
///    astrometry (survey frames like SkyMapper ship with CTYPE/CRVAL/CD but
///    no OBJCTRA/RA). Read from the `fits_header` blob via the existing
///    snapshot path, so frames scanned before the scanner had CRVAL
///    extraction don't need a rescan to get a position hint.
/// 4. Nearby solved frame in the same directory.
///
/// In all branches, sentinel values (NULL, exact 0/0, or sexagesimal
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

    // Stored-header WCS fallback: re-parse the `fits_header` blob and pull
    // ra/dec from its snapshot (which has the CTYPE-guarded CRVAL1/CRVAL2
    // fallback from `stored_header::snapshot_from_keys`). Closes the gap for
    // frames whose scanner run predated the scanner's CRVAL extraction in
    // `fits_parser/mod.rs:292`: those rows have `frame.ra/dec = NULL` even
    // though the WCS sits in the stored header — without this branch they'd
    // solve blindly. Only the (ra,dec) PAIR is taken — partial fills don't
    // make sense.
    if hints.ra.is_none() && hints.dec.is_none() {
        if let (Some(conn), Some(frame_id)) = (conn, frame.id) {
            if let Ok(snaps) = crate::db::get_frame_metadata_originals(conn, &[frame_id]) {
                if let Some(snap) = snaps.into_iter().next() {
                    if let (Some(ra), Some(dec)) = (snap.ra, snap.dec) {
                        if !is_sentinel_position(ra, dec) {
                            hints.ra = Some(ra);
                            hints.dec = Some(dec);
                        }
                    }
                }
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

    // Compute pixel scale and FOV from optics.
    //
    // FITS `XPIXSZ` is the **effective** pixel pitch of a pixel in the *saved*
    // image: every mainstream capture program (ASIAIR, N.I.N.A., SharpCap,
    // ZWO/ASCOM — and established external solvers assume the same) already
    // folds binning into XPIXSZ, with XBINNING kept purely informational.
    // Multiplying XPIXSZ by XBINNING double-counts binning. Empirically
    // (external-oracle bench): a ZWO ASI294MM bin-2 frame reports XPIXSZ=4.63
    // with NAXIS1=4144 — already the 4.63 µm/px of the 4144-px image; ×2
    // yields 1.91"/px vs the true 0.955"/px, breaking the scale hint at long
    // focal length. So use XPIXSZ directly, per the established convention
    // (scale ≈ 206.265·XPIXSZµm / FOCALLENmm).
    if let (Some(focallen), Some(xpixsz)) = (frame.focallen, frame.xpixsz) {
        if focallen > 0.0 && xpixsz > 0.0 {
            let pixel_size_mm = xpixsz / 1000.0;
            hints.pixel_scale_arcsec =
                Some((pixel_size_mm / focallen).atan().to_degrees() * 3600.0);
        }
    }
    hints.fov_deg = frame_fov_deg(frame);

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

    /// Survey-WCS fallback (user-reported case, SkyMapper `1_18_r_1_P.fit`):
    /// the catalog has `frame.ra = NULL` / `frame.dec = NULL` and
    /// `objctra / objctdec` are also missing, but the stored fits_header
    /// blob carries `CTYPE1 = 'RA---TAN'` + `CRVAL1 = 234.42` (and
    /// `CTYPE2='DEC--TAN'` + `CRVAL2 = -34.14`). Without the
    /// stored-header fallback, the solver runs blind across the whole
    /// sky; with it, it gets a 10° hinted search.
    #[test]
    fn stored_header_crval_used_when_objctra_and_ra_both_null() {
        use rusqlite::Connection;
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        // Insert a file row + frame row with no objctra/ra populated.
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format, created_at)
             VALUES ('/x/sky.fits', 'sky.fits', 0, '2026-01-01T00:00:00Z', 'FITS', '2026-01-01T00:00:00Z')",
            [],
        ).unwrap();
        let file_id: i64 = conn
            .query_row("SELECT id FROM files WHERE path = '/x/sky.fits'", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO frames (file_id, imagetyp) VALUES (?1, 'Light')",
            [file_id],
        ).unwrap();
        let frame_id: i64 = conn
            .query_row("SELECT id FROM frames WHERE file_id = ?1", [file_id], |r| r.get(0))
            .unwrap();
        // Seed the fits_header blob with CTYPE-guarded CRVAL only — no
        // OBJCTRA/RA. The scanner-style 80-char card text format works for
        // FITS files via `parse_fits_card_text`.
        let card = |k: &str, v: &str| format!("{:<80}", format!("{:<8}= {:<20}", k, v));
        let blob = [
            card("CTYPE1", "'RA---TAN'"),
            card("CTYPE2", "'DEC--TAN'"),
            card("CRVAL1", "234.42"),
            card("CRVAL2", "-34.14"),
        ]
        .join("\n");
        crate::db::insert_fits_header(&conn, file_id, &blob).unwrap();

        let frame = Frame {
            id: Some(frame_id),
            file_id,
            ..frame_with(None, None, None, None)
        };
        let hints = extract_hints(&frame, Some(&conn));
        assert_eq!(hints.ra, Some(234.42), "CRVAL1 must fill ra hint");
        assert_eq!(hints.dec, Some(-34.14), "CRVAL2 must fill dec hint");
    }

    /// Sanity: the new fallback must NOT fire when frame.id is absent
    /// (newly-constructed Frame with no DB identity) — passing `0` as a
    /// frame_id would return an empty snapshot list and gracefully fall
    /// through to "no hint".
    #[test]
    fn stored_header_fallback_skipped_without_frame_id() {
        let frame = frame_with(None, None, None, None);
        // No conn at all → no fallback.
        let hints = extract_hints(&frame, None);
        assert_eq!(hints.ra, None);
        assert_eq!(hints.dec, None);
    }

    #[test]
    fn fov_from_optics_matches_formula_and_guards() {
        // 270mm, 3.76µm, 6248px → ~4.98° (ASI2600 at a short focal length).
        let fov = fov_from_optics(Some(270.0), Some(3.76), Some(6248)).unwrap();
        assert!((fov - 4.98).abs() < 0.02, "got {fov}");
        // Missing or non-positive inputs → None.
        assert_eq!(fov_from_optics(None, Some(3.76), Some(6248)), None);
        assert_eq!(fov_from_optics(Some(0.0), Some(3.76), Some(6248)), None);
        assert_eq!(fov_from_optics(Some(270.0), Some(3.76), Some(0)), None);
    }

    #[test]
    fn fov_summary_takes_global_narrowest_and_counts() {
        let rows = vec![
            (Some(270.0), Some(3.76), Some(6248), Some("ASI2600".to_string())),   // ~4.98°
            (Some(2491.0), Some(3.76), Some(2048), Some("SG_32".to_string())),    // ~0.18° narrowest
            (None, None, None, Some("NoOptics".to_string())),                     // not computable
        ];
        let s = fov_summary(rows);
        assert_eq!(s.light_count, 3);
        assert_eq!(s.computable_count, 2);
        assert!((s.min_fov_deg.unwrap() - 0.18).abs() < 0.02, "{:?}", s.min_fov_deg);
        assert_eq!(s.narrowest_instrume.as_deref(), Some("SG_32"));
    }

    #[test]
    fn fov_summary_none_when_nothing_computable() {
        let s = fov_summary(vec![(None, None, None, Some("x".to_string()))]);
        assert_eq!(s.light_count, 1);
        assert_eq!(s.computable_count, 0);
        assert_eq!(s.min_fov_deg, None);
        assert_eq!(s.narrowest_instrume, None);
    }
}

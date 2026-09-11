use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use solvemyastro::wcs::{SipCoefficients, WcsSolution};
use tracing::warn;

use crate::coordinates::{format_dec_sexagesimal, format_ra_sexagesimal};

/// A stored plate solve result, mirroring the plate_solves table.
#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS)]
pub struct PlateSolveRecord {
    pub id: Option<i64>,
    pub frame_id: i64,
    pub crpix1: f64,
    pub crpix2: f64,
    pub crval1: f64,
    pub crval2: f64,
    pub cd1_1: f64,
    pub cd1_2: f64,
    pub cd2_1: f64,
    pub cd2_2: f64,
    pub sip_order: Option<i32>,
    pub sip_a_coeffs: Option<String>,
    pub sip_b_coeffs: Option<String>,
    pub sip_ap_coeffs: Option<String>,
    pub sip_bp_coeffs: Option<String>,
    pub matched_stars: i32,
    pub total_detected: i32,
    pub rms_residual_px: f64,
    pub rms_residual_arcsec: f64,
    pub pixel_scale_arcsec: f64,
    pub field_rotation_deg: f64,
    pub solve_time_ms: i64,
    pub catalog_used: String,
    pub algorithm_used: String,
    pub solved_at: String,
    /// Number of catalog stars that fell within the solved field of view.
    /// Used by the density-aware acceptance gate. None for pre-density-aware
    /// solves stored before the migration.
    pub expected_catalog_stars_in_fov: Option<i32>,
    /// matched_stars / expected_catalog_stars_in_fov — solve confidence
    /// signal independent of absolute star count. Closer to 1.0 = stronger
    /// match. None for pre-density-aware solves.
    pub inlier_ratio: Option<f64>,
}

impl PlateSolveRecord {
    /// The stored row as the [`WcsSolution`] the solver produced (M4b):
    /// `crpix` 0-based on both sides — no ±1 anywhere — the CD matrix as
    /// stored, and the SIP tables re-parsed from the JSON `service.rs`
    /// wrote (`serde_json::to_string(&coeffs)` of a `Vec<Vec<f64>>`).
    ///
    /// `None` when a stored SIP table fails to parse: a solve whose
    /// distortion we cannot read is not the same solve, and silently
    /// dropping the polynomial would hand the caller a transform that is
    /// wrong by exactly the distortion it was fitted to absorb. A record
    /// with no SIP tables at all is a perfectly good linear solution and
    /// converts.
    pub fn to_solution(&self) -> Option<WcsSolution> {
        let sip_forward = self
            .sip_pair(self.sip_a_coeffs.as_deref(), self.sip_b_coeffs.as_deref())
            .map_err(|e| {
                warn!(frame_id = self.frame_id, error = %e, "stored SIP forward coefficients failed to parse")
            })
            .ok()?;
        let sip_reverse = self
            .sip_pair(self.sip_ap_coeffs.as_deref(), self.sip_bp_coeffs.as_deref())
            .map_err(|e| {
                warn!(frame_id = self.frame_id, error = %e, "stored SIP reverse coefficients failed to parse")
            })
            .ok()?;
        Some(WcsSolution {
            crpix: (self.crpix1, self.crpix2),
            crval: (self.crval1, self.crval2),
            cd: [[self.cd1_1, self.cd1_2], [self.cd2_1, self.cd2_2]],
            sip_forward,
            sip_reverse,
        })
    }

    /// One SIP half-pair. `Ok(None)` = nothing stored (no order, or either
    /// table missing); `Err` = a table is there but unreadable.
    fn sip_pair(
        &self,
        a: Option<&str>,
        b: Option<&str>,
    ) -> std::result::Result<Option<(SipCoefficients, SipCoefficients)>, serde_json::Error> {
        let (order, a, b) = match (self.sip_order, a, b) {
            (Some(order), Some(a), Some(b)) => (order.clamp(0, u8::MAX as i32) as u8, a, b),
            _ => return Ok(None),
        };
        let a: Vec<Vec<f64>> = serde_json::from_str(a)?;
        let b: Vec<Vec<f64>> = serde_json::from_str(b)?;
        Ok(Some((
            SipCoefficients { order, coeffs: a },
            SipCoefficients { order, coeffs: b },
        )))
    }
}

/// Insert or replace a plate solve result.
pub fn insert_plate_solve(conn: &Connection, record: &PlateSolveRecord) -> Result<i64> {
    conn.execute(
        "INSERT OR REPLACE INTO plate_solves (
            frame_id, crpix1, crpix2, crval1, crval2,
            cd1_1, cd1_2, cd2_1, cd2_2,
            sip_order, sip_a_coeffs, sip_b_coeffs, sip_ap_coeffs, sip_bp_coeffs,
            matched_stars, total_detected,
            rms_residual_px, rms_residual_arcsec,
            pixel_scale_arcsec, field_rotation_deg,
            solve_time_ms, catalog_used, algorithm_used, solved_at,
            expected_catalog_stars_in_fov, inlier_ratio
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5,
            ?6, ?7, ?8, ?9,
            ?10, ?11, ?12, ?13, ?14,
            ?15, ?16,
            ?17, ?18,
            ?19, ?20,
            ?21, ?22, ?23, ?24,
            ?25, ?26
        )",
        rusqlite::params![
            record.frame_id,
            record.crpix1,
            record.crpix2,
            record.crval1,
            record.crval2,
            record.cd1_1,
            record.cd1_2,
            record.cd2_1,
            record.cd2_2,
            record.sip_order,
            record.sip_a_coeffs,
            record.sip_b_coeffs,
            record.sip_ap_coeffs,
            record.sip_bp_coeffs,
            record.matched_stars,
            record.total_detected,
            record.rms_residual_px,
            record.rms_residual_arcsec,
            record.pixel_scale_arcsec,
            record.field_rotation_deg,
            record.solve_time_ms,
            record.catalog_used,
            record.algorithm_used,
            record.solved_at,
            record.expected_catalog_stars_in_fov,
            record.inlier_ratio,
        ],
    )
    .context("Failed to insert plate solve record")?;

    let id = conn.last_insert_rowid();
    Ok(id)
}

/// Get plate solve result for a frame.
pub fn get_plate_solve(conn: &Connection, frame_id: i64) -> Result<Option<PlateSolveRecord>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, frame_id, crpix1, crpix2, crval1, crval2,
                    cd1_1, cd1_2, cd2_1, cd2_2,
                    sip_order, sip_a_coeffs, sip_b_coeffs, sip_ap_coeffs, sip_bp_coeffs,
                    matched_stars, total_detected,
                    rms_residual_px, rms_residual_arcsec,
                    pixel_scale_arcsec, field_rotation_deg,
                    solve_time_ms, catalog_used, algorithm_used, solved_at,
                    expected_catalog_stars_in_fov, inlier_ratio
             FROM plate_solves WHERE frame_id = ?1",
        )
        .context("Failed to prepare plate solve query")?;

    let result = stmt.query_row([frame_id], |row| {
        Ok(PlateSolveRecord {
            id: row.get(0)?,
            frame_id: row.get(1)?,
            crpix1: row.get(2)?,
            crpix2: row.get(3)?,
            crval1: row.get(4)?,
            crval2: row.get(5)?,
            cd1_1: row.get(6)?,
            cd1_2: row.get(7)?,
            cd2_1: row.get(8)?,
            cd2_2: row.get(9)?,
            sip_order: row.get(10)?,
            sip_a_coeffs: row.get(11)?,
            sip_b_coeffs: row.get(12)?,
            sip_ap_coeffs: row.get(13)?,
            sip_bp_coeffs: row.get(14)?,
            matched_stars: row.get(15)?,
            total_detected: row.get(16)?,
            rms_residual_px: row.get(17)?,
            rms_residual_arcsec: row.get(18)?,
            pixel_scale_arcsec: row.get(19)?,
            field_rotation_deg: row.get(20)?,
            solve_time_ms: row.get(21)?,
            catalog_used: row.get(22)?,
            algorithm_used: row.get(23)?,
            solved_at: row.get(24)?,
            expected_catalog_stars_in_fov: row.get(25)?,
            inlier_ratio: row.get(26)?,
        })
    });

    match result {
        Ok(record) => Ok(Some(record)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e).context("Failed to fetch plate solve record"),
    }
}

/// Delete plate solve result for a frame.
pub fn delete_plate_solve(conn: &Connection, frame_id: i64) -> Result<()> {
    conn.execute("DELETE FROM plate_solves WHERE frame_id = ?1", [frame_id])
        .context("Failed to delete plate solve record")?;
    Ok(())
}

/// Update the frames table with all data from a plate solve.
///
/// Coordinates are saved in both numeric and sexagesimal format:
/// - `ra` / `dec`: decimal degrees (f64)
/// - `objctra`: sexagesimal "HH:MM:SS.s" via format_ra_sexagesimal()
/// - `objctdec`: sexagesimal "+DD:MM:SS.s" via format_dec_sexagesimal()
/// - `rotation`: decimal degrees (f64), N through E
/// - `focallen`: written when the frame had NULL (derived from plate solve);
///   also overwrites an existing value when `focallen_is_correction` is true
///   (the solve only succeeded after the wrong header FOCALLEN was discarded).
///   `override = 1` keeps the scanner's non-destructive re-parse from
///   reverting it to the bad header value.
pub fn update_frame_from_solve(
    conn: &Connection,
    frame_id: i64,
    ra_deg: f64,
    dec_deg: f64,
    rotation_deg: f64,
    derived_focallen_mm: Option<f64>,
    focallen_is_correction: bool,
) -> Result<()> {
    let objctra = format_ra_sexagesimal(ra_deg);
    let objctdec = format_dec_sexagesimal(dec_deg);

    conn.execute(
        "UPDATE frames SET ra = ?1, dec = ?2, rotation = ?3, objctra = ?4, objctdec = ?5,
         override = 1 WHERE id = ?6",
        rusqlite::params![ra_deg, dec_deg, rotation_deg, objctra, objctdec, frame_id],
    )
    .context("Failed to update frame coordinates")?;

    if let Some(fl) = derived_focallen_mm {
        // Default: fill only when missing. Correction: overwrite the wrong
        // header value too (the blind solve proved it wrong).
        let sql = if focallen_is_correction {
            "UPDATE frames SET focallen = ?1, override = 1 WHERE id = ?2"
        } else {
            "UPDATE frames SET focallen = ?1, override = 1 WHERE id = ?2 AND focallen IS NULL"
        };
        conn.execute(sql, rusqlite::params![fl, frame_id])
            .context("Failed to update derived focal length")?;
    }

    Ok(())
}

/// Set `frame.object` to `designation` if (and only if) it is currently NULL
/// or empty. Used to auto-label plate-solve results with the nearest DSO.
///
/// Sets `override = 1` so the row carries the same "manually edited" marker
/// the dual-pane editor uses — making the auto-fill visible in the file
/// browser and reachable through the existing revert UI in MetadataPane.
pub fn update_frame_object_if_missing(
    conn: &Connection,
    frame_id: i64,
    designation: &str,
) -> Result<bool> {
    let changed = conn
        .execute(
            "UPDATE frames SET object = ?1, override = 1
             WHERE id = ?2 AND (object IS NULL OR object = '')",
            rusqlite::params![designation, frame_id],
        )
        .context("Failed to update frame object")?;
    Ok(changed > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A record carrying the M4b test WCS: 0-based `crpix`, a CD matrix and
    /// whichever SIP tables the caller supplies.
    fn record(
        sip_a: Option<&str>,
        sip_b: Option<&str>,
        sip_order: Option<i32>,
    ) -> PlateSolveRecord {
        PlateSolveRecord {
            id: None,
            frame_id: 77,
            crpix1: 1234.5,
            crpix2: 678.9,
            crval1: 83.82,
            crval2: -5.39,
            cd1_1: 5.5e-5,
            cd1_2: 1.1e-7,
            cd2_1: -1.1e-7,
            cd2_2: 5.5e-5,
            sip_order,
            sip_a_coeffs: sip_a.map(str::to_string),
            sip_b_coeffs: sip_b.map(str::to_string),
            sip_ap_coeffs: None,
            sip_bp_coeffs: None,
            matched_stars: 40,
            total_detected: 120,
            rms_residual_px: 0.4,
            rms_residual_arcsec: 0.1,
            pixel_scale_arcsec: 0.78,
            field_rotation_deg: 0.0,
            solve_time_ms: 10,
            catalog_used: "test".to_string(),
            algorithm_used: "test".to_string(),
            solved_at: "2025-01-01T00:00:00Z".to_string(),
            expected_catalog_stars_in_fov: None,
            inlier_ratio: None,
        }
    }

    #[test]
    fn to_solution_rebuilds_the_wcs_and_its_sip_tables() {
        let a = serde_json::to_string(&vec![vec![0.0, 0.0, 1e-6], vec![0.0, 2e-6], vec![3e-6]])
            .unwrap();
        let b = serde_json::to_string(&vec![vec![0.0, 0.0, 4e-6], vec![0.0, 5e-6], vec![6e-6]])
            .unwrap();
        let rec = record(Some(&a), Some(&b), Some(2));
        let w = rec.to_solution().expect("a well-formed record converts");

        assert_eq!(w.crpix, (1234.5, 678.9));
        assert_eq!(w.crval, (83.82, -5.39));
        assert_eq!(w.cd, [[5.5e-5, 1.1e-7], [-1.1e-7, 5.5e-5]]);
        // The reference pixel projects back onto CRVAL (SIP is zero there).
        let (ra, dec) = w.pixel_to_sky(1234.5, 678.9);
        assert!(
            (ra - 83.82).abs() < 1e-9 && (dec + 5.39).abs() < 1e-9,
            "{ra} {dec}"
        );

        let (fa, fb) = w.sip_forward.as_ref().expect("forward SIP present");
        assert_eq!((fa.order, fb.order), (2, 2));
        assert_eq!(fa.coeffs[0][2], 1e-6);
        assert_eq!(fb.coeffs[2][0], 6e-6);
        assert!(w.sip_reverse.is_none(), "no reverse tables stored");
    }

    #[test]
    fn to_solution_is_none_on_unparseable_sip_and_sip_less_records_still_convert() {
        assert!(record(Some("not json"), Some("[[0.0]]"), Some(2))
            .to_solution()
            .is_none());
        assert!(record(Some("[[0.0]]"), Some("not json"), Some(2))
            .to_solution()
            .is_none());
        // No SIP at all is a perfectly good linear solution.
        let plain = record(None, None, None)
            .to_solution()
            .expect("linear solve");
        assert!(plain.sip_forward.is_none() && plain.sip_reverse.is_none());
        // An order with no tables behind it is likewise just linear.
        let orphan = record(None, None, Some(3))
            .to_solution()
            .expect("linear solve");
        assert!(orphan.sip_forward.is_none());
    }
}

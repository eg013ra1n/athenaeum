//! One canonical `frames` row → [`Frame`] mapper for the calibration group
//! detectors.
//!
//! `dark_bias_groups` (dark + bias) and `flat_groups` each used to inline the
//! same 68-line mapper next to its own hand-written SELECT list. Three copies
//! of one column order is three chances to drift, and they had already drifted
//! in the same way: all three hard-coded `bayerpat`/`xbayroff`/`ybayroff`/
//! `roworder` to `None` because none of the SELECT lists asked for those
//! columns — the same defect `420bde27` fixed in the light-frame and
//! representative-frame loaders.
//!
//! The column list and the index mapping now live in exactly one place:
//! [`GROUP_FRAME_SELECT_COLUMNS`] is the only SELECT list the three detectors
//! use, and [`frame_from_group_row`] is the only thing that reads it. Each
//! detector still builds its own WHERE / parameter binding — only the
//! projection and the decode are shared.

use chrono::{DateTime, Utc};

use crate::models::{Frame, ImageType};

/// Projection shared by every calibration-group detector query.
///
/// The order is load-bearing: [`frame_from_group_row`] reads by index. Append
/// new columns at the end (and read them at the new index) — never reorder.
pub(crate) const GROUP_FRAME_SELECT_COLUMNS: &str =
    "id, file_id, object, date_obs, telescop, instrume, exptime, filter, imagetyp,
                is_master, ra, dec, objctra, objctdec, gain, offset, xbinning, ybinning,
                ccd_temp, set_temp, focallen, xpixsz, ypixsz, naxis1, naxis2,
                sitelat, lat_obs, sitelong, long_obs, uuid, updated_at,
                bayerpat, xbayroff, ybayroff, roworder";

/// Decodes one row of [`GROUP_FRAME_SELECT_COLUMNS`] into a [`Frame`].
///
/// `override_`, `swcreate` and `rotation` are not part of the projection and
/// stay at their neutral defaults — the group detectors cluster on time,
/// temperature and the matching parameters, and never consult those three.
/// The CFA fields are different: they are read from the row, because the
/// matcher's mono/ambiguous-filter check consults `bayerpat` and a colour
/// calibration frame must not be mistaken for a mono one. Absent values decode
/// to `None` — never fabricated.
pub(crate) fn frame_from_group_row(row: &rusqlite::Row) -> rusqlite::Result<Frame> {
    let date_obs_str: Option<String> = row.get(3)?;
    let date_obs = date_obs_str
        .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
        .map(|dt| dt.with_timezone(&Utc));

    let imagetyp_str: Option<String> = row.get(8)?;
    let imagetyp = imagetyp_str.and_then(|s| ImageType::from_str(&s));

    let xbinning: Option<i32> = row.get(16)?;
    let ybinning: Option<i32> = row.get(17)?;
    let binning = match (xbinning, ybinning) {
        (Some(x), Some(y)) => Some(format!("{}x{}", x, y)),
        _ => None,
    };

    let is_master_int: i32 = row.get(9)?;

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
        is_master: is_master_int != 0,
        gain: row.get(14)?,
        offset: row.get(15)?,
        binning,
        xbinning,
        ybinning,
        ccd_temp: row.get(18)?,
        set_temp: row.get(19)?,
        focallen: row.get(20)?,
        xpixsz: row.get(21)?,
        ypixsz: row.get(22)?,
        naxis1: row.get(23)?,
        naxis2: row.get(24)?,
        ra: row.get(10)?,
        dec: row.get(11)?,
        sitelat: row.get(25)?,
        lat_obs: row.get(26)?,
        sitelong: row.get(27)?,
        long_obs: row.get(28)?,
        objctra: row.get(12)?,
        objctdec: row.get(13)?,
        override_: false,
        swcreate: None,
        bayerpat: row.get(31)?,
        xbayroff: row.get(32)?,
        ybayroff: row.get(33)?,
        roworder: row.get(34)?,
        rotation: None,
        uuid: row.get(29)?,
        updated_at: row.get(30)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// The mapper's contract against the real schema: every column of the
    /// shared projection decodes into the field it names, and the four CFA
    /// columns in particular arrive populated rather than assumed away.
    #[test]
    fn maps_every_projected_column_including_cfa() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (1, '/data/dark.fits', 'dark.fits', 1024, '2025-09-25T00:00:00+00:00', 'FITS')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames
             (id, file_id, object, date_obs, telescop, instrume, exptime, filter, imagetyp,
              is_master, gain, offset, binning, xbinning, ybinning, ccd_temp, set_temp,
              focallen, uuid, bayerpat, xbayroff, ybayroff, roworder)
             VALUES (1, 1, 'M42', '2025-09-25T00:00:00+00:00', 'RC8', 'ASI2600MC', 300.0, NULL,
                     'Dark', 0, 56.0, 50.0, '1x1', 1, 1, -10.0, -10.0,
                     1600.0, 'frame-uuid', 'RGGB', 1, 0, 'BOTTOM-UP')",
            [],
        )
        .unwrap();

        let query = format!(
            "SELECT {} FROM frames WHERE id = 1",
            GROUP_FRAME_SELECT_COLUMNS
        );
        let frame = conn
            .query_row(&query, [], |row| frame_from_group_row(row))
            .unwrap();

        assert_eq!(frame.id, Some(1));
        assert_eq!(frame.file_id, 1);
        assert_eq!(frame.object.as_deref(), Some("M42"));
        assert_eq!(frame.instrume.as_deref(), Some("ASI2600MC"));
        assert_eq!(frame.exptime, Some(300.0));
        assert_eq!(frame.imagetyp, Some(ImageType::Dark));
        assert!(!frame.is_master);
        assert_eq!(frame.binning.as_deref(), Some("1x1"));
        assert_eq!(frame.ccd_temp, Some(-10.0));
        assert_eq!(frame.uuid.as_deref(), Some("frame-uuid"));
        assert!(frame.date_obs.is_some(), "date_obs must parse from RFC3339");

        assert_eq!(frame.bayerpat.as_deref(), Some("RGGB"));
        assert_eq!(frame.xbayroff, Some(1));
        assert_eq!(frame.ybayroff, Some(0));
        assert_eq!(frame.roworder.as_deref(), Some("BOTTOM-UP"));
        assert!(
            !crate::models::is_mono_with_ambiguous_filter(&frame.bayerpat, &frame.filter),
            "a calibration frame declaring a CFA pattern must never look like filter-ambiguous mono"
        );
    }

    /// A mono frame keeps `None` for all four — the columns are read, never
    /// invented.
    #[test]
    fn absent_cfa_columns_stay_none() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (2, '/data/mono.fits', 'mono.fits', 1024, '2025-09-25T00:00:00+00:00', 'FITS')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, is_master) VALUES (2, 2, 'Bias', 0)",
            [],
        )
        .unwrap();

        let query = format!(
            "SELECT {} FROM frames WHERE id = 2",
            GROUP_FRAME_SELECT_COLUMNS
        );
        let frame = conn
            .query_row(&query, [], |row| frame_from_group_row(row))
            .unwrap();

        assert!(frame.bayerpat.is_none());
        assert!(frame.xbayroff.is_none());
        assert!(frame.ybayroff.is_none());
        assert!(frame.roworder.is_none());
        assert!(
            frame.binning.is_none(),
            "no xbinning/ybinning → no composed binning"
        );
    }
}

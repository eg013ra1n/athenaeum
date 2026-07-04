//! Consolidate a master's FITS header from its source calibration set +
//! member frames (spec §3 step 3, arch-doc B3).

use crate::fits_parser::stored_header::parse_stored_header_keys;
use crate::fits_writer::keywords::{Bayer, FrameKind, HeaderBuilder};
use crate::fits_writer::{Card, FitsWriteError};
use crate::models::FileFormat;
use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use rusqlite::Connection;

/// Everything needed to consolidate a master header from its source set.
/// Loaded with [`load_header_inputs`].
pub struct MasterHeaderInputs {
    pub kind: FrameKind,
    pub instrume: Option<String>,
    pub telescop: Option<String>,
    pub filter: Option<String>,
    pub exptime: Option<f64>,
    pub gain: Option<f64>,
    pub offset: Option<f64>,
    pub xbinning: Option<i64>,
    pub ybinning: Option<i64>,
    pub xpixsz: Option<f64>,
    pub ypixsz: Option<f64>,
    pub focallen: Option<f64>,
    pub egain: Option<f64>,
    pub bayerpat: Option<String>,
    pub xbayroff: Option<i64>,
    pub ybayroff: Option<i64>,
    pub temp_mean: Option<f64>,
    pub temp_min: Option<f64>,
    pub temp_max: Option<f64>,
    pub date_obs_midpoint: Option<DateTime<Utc>>,
    pub frame_count: u32,
    pub source_set_uuid: String,
}

fn master_kind_for(imagetyp: &str) -> Option<FrameKind> {
    match imagetyp {
        "Dark" | "MasterDark" => Some(FrameKind::MasterDark),
        "Flat" | "MasterFlat" => Some(FrameKind::MasterFlat),
        "Bias" | "MasterBias" => Some(FrameKind::MasterBias),
        "DarkFlat" | "MasterDarkFlat" => Some(FrameKind::MasterDarkFlat),
        _ => None,
    }
}

pub fn load_header_inputs(conn: &Connection, source_set_id: i64) -> Result<MasterHeaderInputs> {
    // Set-level values (already aggregated by the scanner's clustering).
    // `binning` (calibration_set's text summary, e.g. "1x1") is intentionally
    // unused here: the numeric xbinning/ybinning consumed by MasterHeaderInputs
    // come from the frame-level aggregate query below instead.
    let (imagetyp, exptime, filter, ccd_temp, gain, offset, _binning, instrume, telescop,
         temp_min, temp_max, frame_count, focallen, uuid): (
        String, Option<f64>, Option<String>, Option<f64>, Option<f64>, Option<f64>,
        Option<String>, Option<String>, Option<String>, Option<f64>, Option<f64>,
        i64, Option<f64>, String,
    ) = conn.query_row(
        "SELECT imagetyp, exptime, filter, ccd_temp, gain, offset, binning, instrume,
                telescop, temp_min, temp_max, frame_count, focallen, uuid
         FROM calibration_set WHERE id = ?1",
        [source_set_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?,
                r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?, r.get(11)?,
                r.get(12)?, r.get(13)?)),
    )?;
    let kind = master_kind_for(&imagetyp)
        .ok_or_else(|| anyhow!("set {source_set_id} has non-calibration imagetyp {imagetyp}"))?;

    // Frame-level aggregates: temp mean, date midpoint, binning ints, pixel size.
    // Exactly the 7 real frame aggregates — BAYERPAT is NOT one of them (frames
    // has no bayerpat column); it is fetched separately below from the raw
    // stored header of a member file.
    let (temp_mean, min_dt, max_dt, xbin, ybin, xpixsz, ypixsz): (
        Option<f64>, Option<String>, Option<String>, Option<i64>, Option<i64>,
        Option<f64>, Option<f64>,
    ) = conn.query_row(
        "SELECT AVG(f.ccd_temp), MIN(f.date_obs), MAX(f.date_obs),
                MAX(f.xbinning), MAX(f.ybinning), MAX(f.xpixsz), MAX(f.ypixsz)
         FROM calibration_set_frames csf
         JOIN frames f ON f.id = csf.frame_id
         WHERE csf.set_id = ?1",
        [source_set_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
    )?;
    // frames has no bayerpat column — BAYERPAT lives in the stored raw
    // header. Fetch it from fits_header of the first member file, joining
    // files for the format: fits_header.header stores three shapes (FITS
    // 80-col cards, raw XISF XML, ASIAIR-style "KEY = value" dumps), and
    // parse_stored_header_keys is the format-aware accessor that handles
    // all of them (same call pattern as db::operations'
    // clear_override_for_unchanged_frames / get_frame_metadata_originals).
    let bayerpat: Option<String> = conn
        .query_row(
            "SELECT fi.format, fh.header FROM calibration_set_frames csf
             JOIN frames f ON f.id = csf.frame_id
             JOIN files fi ON fi.id = f.file_id
             JOIN fits_header fh ON fh.file_id = f.file_id
             WHERE csf.set_id = ?1 LIMIT 1",
            [source_set_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .ok()
        .and_then(|(format_str, header)| {
            let format = match format_str.as_str() {
                "FITS" => FileFormat::FITS,
                "XISF" => FileFormat::XISF,
                _ => return None, // Unknown format — skip rather than guess.
            };
            // Returned map is keyed UPPERCASE (see parse_stored_header_keys docs).
            parse_stored_header_keys(format, &header).remove("BAYERPAT")
        })
        .filter(|s| !s.trim().is_empty());

    let midpoint = match (parse_dt(min_dt.as_deref()), parse_dt(max_dt.as_deref())) {
        (Some(a), Some(b)) => Some(a + (b - a) / 2),
        (Some(a), None) => Some(a),
        _ => None,
    };

    Ok(MasterHeaderInputs {
        kind,
        instrume, telescop, filter, exptime,
        gain, offset,
        xbinning: xbin, ybinning: ybin,
        xpixsz, ypixsz, focallen,
        egain: None, // EGAIN is not columnized; omitted from masters (additive later)
        bayerpat,
        xbayroff: None, ybayroff: None,
        temp_mean: temp_mean.or(ccd_temp),
        temp_min, temp_max,
        date_obs_midpoint: midpoint,
        frame_count: frame_count as u32,
        source_set_uuid: uuid,
    })
}

fn parse_dt(s: Option<&str>) -> Option<DateTime<Utc>> {
    s.and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map(|d| d.with_timezone(&Utc))
}

pub fn build_master_cards(
    inputs: &MasterHeaderInputs,
    app_version: &str,
    recipe_summary: &str, // e.g. "winsorized(3.0,3.0) n=24"
    member_hash: &str,
    flat_norm: Option<f64>, // stamps ATH_FNRM when Some
) -> std::result::Result<Vec<Card>, FitsWriteError> {
    let mut b = HeaderBuilder::new(inputs.kind).swcreate(app_version);
    if let Some(v) = inputs.exptime {
        b = b.exptime(v);
    }
    if let Some(dt) = inputs.date_obs_midpoint {
        b = b.date_obs(dt);
    }
    if let Some(t) = inputs.temp_mean {
        b = b.ccd_temp(t);
    }
    if let Some(g) = inputs.gain {
        b = b.gain(g.round() as i64);
    }
    if let Some(o) = inputs.offset {
        b = b.offset(o.round() as i64);
    }
    if let (Some(x), Some(y)) = (inputs.xbinning, inputs.ybinning) {
        b = b.binning(x, y);
    }
    if let (Some(x), Some(y)) = (inputs.xpixsz, inputs.ypixsz) {
        b = b.pixel_size(x, y);
    }
    if let Some(v) = &inputs.instrume {
        b = b.instrume(v);
    }
    if let Some(v) = &inputs.telescop {
        b = b.telescop(v);
    }
    if let Some(v) = inputs.focallen {
        b = b.focallen(v);
    }
    if let Some(v) = &inputs.filter {
        b = b.filter(v);
    }
    if let Some(p) = &inputs.bayerpat {
        let bayer = match p.to_ascii_uppercase().as_str() {
            "RGGB" => Some(Bayer::Rggb),
            "BGGR" => Some(Bayer::Bggr),
            "GBRG" => Some(Bayer::Gbrg),
            "GRBG" => Some(Bayer::Grbg),
            _ => None,
        };
        if let Some(bp) = bayer {
            b = b.bayer(bp, inputs.xbayroff.unwrap_or(0), inputs.ybayroff.unwrap_or(0));
        }
    }
    b = b
        .ath_src(&inputs.source_set_uuid)
        .ath_n(inputs.frame_count)
        .ath_rej(recipe_summary)
        .ath_ver(app_version)
        .ath_hsh(member_hash);
    if let (Some(min), Some(max)) = (inputs.temp_min, inputs.temp_max) {
        b = b.ath_temp_span(min, max);
    }
    if let Some(n) = flat_norm {
        b = b.custom(
            Card::new("ATH_FNRM", crate::fits_writer::CardValue::Real(n))?
                .with_comment("central-third mean of this master flat"),
        );
    }
    b.build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::keywords::FrameKind;
    use rusqlite::Connection;

    fn seed(conn: &Connection) -> i64 {
        crate::db::schema::init_db(conn).unwrap();
        conn.execute(
            "INSERT INTO calibration_set
             (imagetyp, exptime, ccd_temp, gain, offset, binning, instrume, telescop,
              date, date_start, date_end, temp_min, temp_max, frame_count, focallen)
             VALUES ('Dark', 300.0, -10.0, 100.0, 50.0, '1x1', 'TestCam', 'TestScope',
              '2026-06-28', '2026-06-28T20:00:00Z', '2026-06-28T22:00:00Z',
              -10.6, -9.4, 2, 540.0)",
            [],
        ).unwrap();
        let set_id = conn.last_insert_rowid();
        for (i, dt) in ["2026-06-28T20:00:00Z", "2026-06-28T22:00:00Z"].iter().enumerate() {
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format)
                 VALUES (?1, ?2, 10, '2026-06-28', 'FITS')",
                rusqlite::params![format!("/d/f{i}.fits"), format!("f{i}.fits")],
            ).unwrap();
            let file_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO frames (file_id, imagetyp, instrume, exptime, gain, offset,
                                     binning, xbinning, ybinning, ccd_temp, date_obs, xpixsz, ypixsz)
                 VALUES (?1, 'Dark', 'TestCam', 300.0, 100.0, 50.0, '1x1', 1, 1, ?2, ?3, 3.76, 3.76)",
                rusqlite::params![file_id, -10.0 - (i as f64) * 0.5, dt],
            ).unwrap();
            let frame_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                rusqlite::params![set_id, frame_id],
            ).unwrap();
        }
        set_id
    }

    #[test]
    fn consolidated_cards_cover_the_vocabulary() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        let inputs = load_header_inputs(&conn, set_id).unwrap();
        assert_eq!(inputs.kind, FrameKind::MasterDark);
        assert_eq!(inputs.frame_count, 2);
        // midpoint of 20:00 and 22:00 is 21:00
        assert_eq!(inputs.date_obs_midpoint.unwrap().to_rfc3339(), "2026-06-28T21:00:00+00:00");
        let cards = build_master_cards(&inputs, "0.2.5", "winsorized(3.0,3.0) n=2", "cafe", None).unwrap();
        let find = |k: &str| cards.iter().find(|c| c.keyword == k);
        assert!(find("IMAGETYP").is_some());
        assert!(find("INSTRUME").is_some());
        assert!(find("EXPTIME").is_some());
        assert!(find("CCD-TEMP").is_some());
        assert!(find("ATH_TMIN").is_some() && find("ATH_TMAX").is_some());
        assert!(find("ATH_SRC").is_some());
        assert!(find("ATH_N").is_some());
        assert!(find("ATH_REJ").is_some());
        assert!(find("ATH_HSH").is_some());
        assert!(find("SWCREATE").is_some());
        assert!(find("ATH_FNRM").is_none(), "darks carry no flat norm");
    }

    #[test]
    fn flat_norm_card_present_for_flats() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        conn.execute("UPDATE calibration_set SET imagetyp='Flat', filter='L' WHERE id=?1", [set_id]).unwrap();
        let inputs = load_header_inputs(&conn, set_id).unwrap();
        assert_eq!(inputs.kind, FrameKind::MasterFlat);
        let cards = build_master_cards(&inputs, "0.2.5", "percentile(0.2,0.02) n=2", "cafe", Some(1234.5)).unwrap();
        let f = cards.iter().find(|c| c.keyword == "ATH_FNRM").expect("ATH_FNRM");
        assert!(matches!(f.value, Some(crate::fits_writer::CardValue::Real(v)) if (v - 1234.5).abs() < 1e-9));
        assert!(cards.iter().any(|c| c.keyword == "FILTER"));
    }

    /// Attach the same stored-header blob (and format) to every member file
    /// of the set — the BAYERPAT lookup uses `LIMIT 1` with no ORDER BY, so
    /// seeding all members keeps the test deterministic regardless of which
    /// row SQLite returns first.
    fn seed_stored_headers(conn: &Connection, set_id: i64, format: &str, header: &str) {
        conn.execute(
            &format!(
                "UPDATE files SET format = ?1 WHERE id IN (
                     SELECT f.file_id FROM calibration_set_frames csf
                     JOIN frames f ON f.id = csf.frame_id WHERE csf.set_id = {set_id})"
            ),
            [format],
        ).unwrap();
        conn.execute(
            &format!(
                "INSERT INTO fits_header (file_id, header)
                 SELECT f.file_id, ?1 FROM calibration_set_frames csf
                 JOIN frames f ON f.id = csf.frame_id WHERE csf.set_id = {set_id}"
            ),
            [header],
        ).unwrap();
    }

    fn assert_bayer_consolidated(conn: &Connection, set_id: i64) {
        let inputs = load_header_inputs(conn, set_id).unwrap();
        assert_eq!(inputs.bayerpat.as_deref(), Some("RGGB"));
        let cards = build_master_cards(&inputs, "0.2.5", "winsorized(3.0,3.0) n=2", "cafe", None).unwrap();
        let find = |k: &str| cards.iter().find(|c| c.keyword == k);
        let bp = find("BAYERPAT").expect("BAYERPAT card");
        assert!(
            matches!(&bp.value, Some(crate::fits_writer::CardValue::Str(v)) if v == "RGGB"),
            "{:?}", bp.value
        );
        assert!(find("XBAYROFF").is_some(), "XBAYROFF card");
        assert!(find("YBAYROFF").is_some(), "YBAYROFF card");
    }

    #[test]
    fn bayerpat_from_fits_card_header() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        // FITS shape: 80-col cards joined by \n (FitsHeader::to_header_text).
        let header = format!(
            "{:<80}\n{:<80}\n{:<80}",
            "BAYERPAT= 'RGGB    '           / Bayer color pattern",
            "EXPTIME =                300.0 / Exposure time in seconds",
            "END",
        );
        seed_stored_headers(&conn, set_id, "FITS", &header);
        assert_bayer_consolidated(&conn, set_id);
    }

    #[test]
    fn bayerpat_from_asiair_plain_dump() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        // ASIAIR shape: plain "KEY = value" dump. parse_fits_card_text must
        // yield NOTHING for this blob so the dispatcher falls through to
        // parse_keyword_eq_text — that requires every keyword be 8 chars
        // (a <=7-char key like CREATOR puts "= " at cols 8-10 and would
        // accidentally parse as a FITS card, masking the fallback path).
        let dump = "Captured FITS Keywords:\n=======================\n\nBAYERPAT = RGGB\nXBINNING = 1\n";
        seed_stored_headers(&conn, set_id, "FITS", dump);
        assert_bayer_consolidated(&conn, set_id);
    }

    #[test]
    fn bayerpat_from_xisf_xml_header() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        // XISF shape: raw XML blob with FITSKeyword elements (adapted from
        // stored_header.rs's parses_xisf_xml_fitskeyword_elements fixture).
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<xisf>
  <FITSKeyword name="BAYERPAT" value="'RGGB'" comment="Bayer color pattern" />
  <FITSKeyword name="EXPTIME" value="300.0" />
  <FITSKeyword name="XBINNING" value="1" />
</xisf>"#;
        seed_stored_headers(&conn, set_id, "XISF", xml);
        assert_bayer_consolidated(&conn, set_id);
    }
}

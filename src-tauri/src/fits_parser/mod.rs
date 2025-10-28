// FITS/XISF metadata parser module

use crate::models::{Frame, ImageType};
use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDateTime, Utc};
use fitsio::FitsFile;
use std::path::Path;

/// Extract full FITS header as text
pub fn extract_fits_header(path: &Path) -> Result<String> {
    let mut fitsfile = FitsFile::open(path)
        .with_context(|| format!("Failed to open FITS file: {}", path.display()))?;

    let hdu = fitsfile.primary_hdu()?;

    // Read all header keywords and format as text
    let mut header_text = String::new();

    // Try to read common FITS keywords
    let keywords = vec![
        "SIMPLE", "BITPIX", "NAXIS", "NAXIS1", "NAXIS2", "NAXIS3",
        "EXTEND", "BSCALE", "BZERO", "BUNIT",
        "OBJECT", "DATE-OBS", "TIME-OBS", "TELESCOP", "INSTRUME",
        "OBSERVER", "ORIGIN", "REFERENC", "AUTHOR", "DATE",
        "EXPTIME", "EXPOSURE", "FILTER", "IMAGETYP", "FRAME",
        "GAIN", "EGAIN", "OFFSET", "PEDESTAL",
        "XBINNING", "YBINNING", "BINNING",
        "CCD-TEMP", "SET-TEMP", "TEMP", "COOLTEMP",
        "FOCALLEN", "XPIXSZ", "YPIXSZ", "PIXSZ", "PIXSCALE",
        "RA", "DEC", "OBJCTRA", "OBJCTDEC",
        "EQUINOX", "RADESYS", "CTYPE1", "CTYPE2",
        "CRVAL1", "CRVAL2", "CRPIX1", "CRPIX2",
        "CD1_1", "CD1_2", "CD2_1", "CD2_2",
        "CDELT1", "CDELT2", "CROTA1", "CROTA2",
        "SITELAT", "SITELONG", "LAT-OBS", "LONG-OBS", "ALT-OBS",
        "AIRMASS", "ALTITUDE", "AZIMUTH",
        "COMMENT", "HISTORY",
    ];

    for keyword in keywords {
        if let Ok(value) = hdu.read_key::<String>(&mut fitsfile, keyword) {
            header_text.push_str(&format!("{} = {}\n", keyword, value));
        }
    }

    // If header is still empty, at least record that we tried
    if header_text.is_empty() {
        header_text = format!("Header extracted from: {}\n(No standard keywords found)", path.display());
    }

    Ok(header_text)
}

/// Parse FITS file metadata
pub fn parse_fits(path: &Path, file_id: i64) -> Result<Frame> {
    println!("Parsing FITS file: {}", path.display());

    let mut fitsfile = FitsFile::open(path)
        .with_context(|| format!("Failed to open FITS file: {}", path.display()))?;

    let hdu = fitsfile.primary_hdu()?;

    // Extract standard FITS keywords
    let object = read_keyword_string(&mut fitsfile, &hdu, "OBJECT").ok();
    let date_obs_str = read_keyword_string(&mut fitsfile, &hdu, "DATE-OBS").ok();
    let time_obs = read_keyword_string(&mut fitsfile, &hdu, "TIME-OBS").ok();

    println!("  DATE-OBS from FITS: {:?}", date_obs_str);
    println!("  TIME-OBS from FITS: {:?}", time_obs);
    let telescop = read_keyword_string(&mut fitsfile, &hdu, "TELESCOP").ok();
    let instrume = read_keyword_string(&mut fitsfile, &hdu, "INSTRUME").ok();
    let exptime = read_keyword_f64(&mut fitsfile, &hdu, "EXPTIME").ok();
    let filter = read_keyword_string(&mut fitsfile, &hdu, "FILTER").ok();
    let imagetyp_str = read_keyword_string(&mut fitsfile, &hdu, "IMAGETYP").ok();

    // Additional metadata
    let gain = read_keyword_f64(&mut fitsfile, &hdu, "GAIN").ok();
    let offset = read_keyword_f64(&mut fitsfile, &hdu, "OFFSET").ok();
    let xbinning = read_keyword_i32(&mut fitsfile, &hdu, "XBINNING").ok();
    let ybinning = read_keyword_i32(&mut fitsfile, &hdu, "YBINNING").ok();
    let ccd_temp = read_keyword_f64(&mut fitsfile, &hdu, "CCD-TEMP").ok();
    let set_temp = read_keyword_f64(&mut fitsfile, &hdu, "SET-TEMP").ok();
    let focallen = read_keyword_f64(&mut fitsfile, &hdu, "FOCALLEN").ok();

    // Pixel size
    let xpixsz = read_keyword_f64(&mut fitsfile, &hdu, "XPIXSZ").ok();
    let pixsz = read_keyword_f64(&mut fitsfile, &hdu, "PIXSZ").ok();

    // Astronomical coordinates
    let ra = read_keyword_f64(&mut fitsfile, &hdu, "RA").ok();
    let dec = read_keyword_f64(&mut fitsfile, &hdu, "DEC").ok();
    let objctra = read_keyword_string(&mut fitsfile, &hdu, "OBJCTRA").ok();
    let objctdec = read_keyword_string(&mut fitsfile, &hdu, "OBJCTDEC").ok();

    // Observatory location
    let sitelat = read_keyword_f64(&mut fitsfile, &hdu, "SITELAT").ok();
    let lat_obs = read_keyword_f64(&mut fitsfile, &hdu, "LAT-OBS").ok();
    let sitelong = read_keyword_f64(&mut fitsfile, &hdu, "SITELONG").ok();
    let long_obs = read_keyword_f64(&mut fitsfile, &hdu, "LONG-OBS").ok();

    // Parse DATE-OBS
    let date_obs = match (date_obs_str.clone(), time_obs.clone()) {
        (Some(date), time) => {
            match parse_date_obs(&date, time.as_deref()) {
                Ok(dt) => {
                    println!("  Parsed date_obs successfully: {}", dt.to_rfc3339());
                    Some(dt)
                },
                Err(e) => {
                    println!("  Failed to parse date_obs: {}", e);
                    None
                }
            }
        },
        _ => {
            println!("  No DATE-OBS found in FITS header!");
            None
        }
    };

    // Parse IMAGETYP
    let imagetyp = imagetyp_str.and_then(|s| ImageType::from_str(&s));

    // Construct binning string if available
    let binning = match (xbinning, ybinning) {
        (Some(x), Some(y)) => Some(format!("{}x{}", x, y)),
        _ => None,
    };

    Ok(Frame {
        id: None,
        file_id,
        object,
        date_obs,
        telescop,
        instrume,
        exptime,
        filter,
        imagetyp,
        gain,
        offset,
        binning,
        xbinning,
        ybinning,
        ccd_temp,
        set_temp,
        focallen,
        xpixsz,
        pixsz,
        ra,
        dec,
        sitelat,
        lat_obs,
        sitelong,
        long_obs,
        objctra,
        objctdec,
        override_: false,
        calibration_set_id: None,
    })
}

/// Parse XISF file metadata (placeholder)
pub fn parse_xisf(_path: &Path, file_id: i64) -> Result<Frame> {
    // TODO: Implement XISF parsing according to XISF 1.0 specification
    // For now, return a minimal frame
    Ok(Frame {
        id: None,
        file_id,
        object: None,
        date_obs: None,
        telescop: None,
        instrume: None,
        exptime: None,
        filter: None,
        imagetyp: None,
        gain: None,
        offset: None,
        binning: None,
        xbinning: None,
        ybinning: None,
        ccd_temp: None,
        set_temp: None,
        focallen: None,
        xpixsz: None,
        pixsz: None,
        ra: None,
        dec: None,
        sitelat: None,
        lat_obs: None,
        sitelong: None,
        long_obs: None,
        objctra: None,
        objctdec: None,
        override_: false,
        calibration_set_id: None,
    })
}

/// Parse DATE-OBS and optional TIME-OBS into ISO 8601 timestamp
pub fn parse_date_obs(date_obs: &str, time_obs: Option<&str>) -> Result<DateTime<Utc>> {
    // Try parsing ISO 8601 with timezone (YYYY-MM-DDTHH:MM:SS.SSSZ)
    if let Ok(dt) = DateTime::parse_from_rfc3339(date_obs) {
        return Ok(dt.with_timezone(&Utc));
    }

    // Try parsing ISO 8601 without timezone - assume UTC
    if let Ok(naive_dt) = NaiveDateTime::parse_from_str(date_obs, "%Y-%m-%dT%H:%M:%S%.f") {
        return Ok(DateTime::from_naive_utc_and_offset(naive_dt, Utc));
    }

    // Try without fractional seconds
    if let Ok(naive_dt) = NaiveDateTime::parse_from_str(date_obs, "%Y-%m-%dT%H:%M:%S") {
        return Ok(DateTime::from_naive_utc_and_offset(naive_dt, Utc));
    }

    // Try combining DATE-OBS and TIME-OBS
    if let Some(time) = time_obs {
        let datetime_str = format!("{}T{}", date_obs, time);
        if let Ok(dt) = DateTime::parse_from_rfc3339(&datetime_str) {
            return Ok(dt.with_timezone(&Utc));
        }
        // Try without timezone
        if let Ok(naive_dt) = NaiveDateTime::parse_from_str(&datetime_str, "%Y-%m-%dT%H:%M:%S%.f") {
            return Ok(DateTime::from_naive_utc_and_offset(naive_dt, Utc));
        }
    }

    // Try parsing date only (YYYY-MM-DD)
    if let Ok(naive_date) = NaiveDateTime::parse_from_str(
        &format!("{} 00:00:00", date_obs),
        "%Y-%m-%d %H:%M:%S",
    ) {
        return Ok(DateTime::from_naive_utc_and_offset(naive_date, Utc));
    }

    anyhow::bail!("Failed to parse DATE-OBS: {}", date_obs)
}

// Helper functions to read FITS keywords
fn read_keyword_string(fitsfile: &mut FitsFile, hdu: &fitsio::hdu::FitsHdu, key: &str) -> Result<String> {
    let value: String = hdu.read_key(fitsfile, key)?;
    Ok(value.trim().to_string())
}

fn read_keyword_f64(fitsfile: &mut FitsFile, hdu: &fitsio::hdu::FitsHdu, key: &str) -> Result<f64> {
    let value: f64 = hdu.read_key(fitsfile, key)?;
    Ok(value)
}

fn read_keyword_i32(fitsfile: &mut FitsFile, hdu: &fitsio::hdu::FitsHdu, key: &str) -> Result<i32> {
    let value: i32 = hdu.read_key(fitsfile, key)?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_date_obs_iso8601() {
        let dt = parse_date_obs("2024-01-15T20:30:45.123Z", None).unwrap();
        assert_eq!(dt.format("%Y-%m-%d").to_string(), "2024-01-15");
    }

    #[test]
    fn test_parse_date_obs_with_time() {
        let dt = parse_date_obs("2024-01-15", Some("20:30:45")).unwrap();
        assert_eq!(dt.format("%Y-%m-%d").to_string(), "2024-01-15");
    }
}

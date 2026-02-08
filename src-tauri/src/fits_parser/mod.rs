// FITS/XISF metadata parser module

use crate::models::{Frame, ImageType};
use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDateTime, Utc};
use fitsio::FitsFile;
use std::path::Path;

/// Detects if RA is in hours (0-24) or degrees (0-360) and normalizes to degrees
///
/// IMPROVED ALGORITHM: Uses OBJCTRA for verification when available to handle edge cases.
///
/// The FITS standard allows RA in both hours [0, 24) and degrees [0, 360).
/// For values in [0, 24), this is ambiguous without additional context.
///
/// This function uses these strategies:
/// 1. If OBJCTRA is available, parse it and compare with numeric RA to determine units
/// 2. If numeric RA matches OBJCTRA (within 0.1°), it's already in degrees
/// 3. If numeric RA * 15 matches OBJCTRA (within 0.1°), it's in hours → convert
/// 4. If no OBJCTRA, use heuristics: RA < 24 with valid DEC → assume hours
///
/// # Arguments
/// * `ra` - Raw RA value from FITS header
/// * `dec` - Optional DEC value for validation
/// * `objctra` - Optional OBJCTRA string for verification
///
/// # Returns
/// RA in decimal degrees, normalized to [0, 360)
///
/// # Edge Cases
/// - RA=0 works correctly in both hours and degrees (0h = 0°)
/// - RA in [1, 24) is verified against OBJCTRA if available
/// - Without OBJCTRA, assumes hours (common in astronomical FITS)
fn normalize_ra_from_fits(ra: f64, dec: Option<f64>, objctra: Option<&str>) -> f64 {
    // Handle RA >= 24: must be degrees
    if ra >= 24.0 {
        return crate::coordinates::normalize_ra(ra);
    }

    // Handle RA < 0: must be degrees, needs normalization
    if ra < 0.0 {
        return crate::coordinates::normalize_ra(ra);
    }

    // RA is in [0, 24): AMBIGUOUS - could be hours or degrees
    // Use OBJCTRA for verification if available
    if let Some(ra_str) = objctra {
        if let Ok(ra_from_objctra) = crate::coordinates::parse_ra_sexagesimal(ra_str) {
            // Compare numeric RA with parsed OBJCTRA
            let diff_as_degrees = (ra - ra_from_objctra).abs();
            let diff_as_hours = ((ra * 15.0) - ra_from_objctra).abs();

            // If numeric RA already matches OBJCTRA (within 0.1°), it's in degrees
            if diff_as_degrees < 0.1 {
                println!("  Verified RA already in degrees: {:.4}° (matches OBJCTRA)", ra);
                return crate::coordinates::normalize_ra(ra);
            }

            // If numeric RA * 15 matches OBJCTRA (within 0.1°), it's in hours
            if diff_as_hours < 0.1 {
                println!("  Detected RA in hours: {:.4}h → {:.4}° (verified with OBJCTRA)", ra, ra * 15.0);
                return crate::coordinates::normalize_ra(ra * 15.0);
            }

            // Neither match well - use OBJCTRA as ground truth
            println!("  WARNING: RA={:.4} doesn't match OBJCTRA. Using OBJCTRA value: {:.4}°", ra, ra_from_objctra);
            return ra_from_objctra;
        }
    }

    // No OBJCTRA available, use heuristics
    if let Some(d) = dec {
        if d >= -90.0 && d <= 90.0 {
            // Valid DEC suggests these are coordinates, assume hours
            println!("  RA={:.4} in ambiguous range [0,24). Assuming hours → {:.4}°", ra, ra * 15.0);
            return crate::coordinates::normalize_ra(ra * 15.0);
        }
    }

    // No context available, default to hours (astronomical convention)
    println!("  WARNING: RA={:.4} is ambiguous. No verification available. Assuming hours.", ra);
    crate::coordinates::normalize_ra(ra * 15.0)
}

/// Validates and normalizes DEC to [-90, 90] range
fn validate_dec(dec: f64) -> Result<f64, String> {
    if dec < -90.0 || dec > 90.0 {
        // Clamp to valid range and warn
        let clamped = crate::coordinates::normalize_dec(dec);
        println!("  WARNING: Invalid DEC={:.4}° (outside [-90, 90]). Clamped to {:.4}°", dec, clamped);
        Ok(clamped)
    } else {
        Ok(dec)
    }
}

/// Extract full FITS header as text
pub fn extract_fits_header(path: &Path) -> Result<String> {
    crate::logging::log("DEBUG", &format!("Opening FITS for header: {}", path.display()));
    let mut fitsfile = FitsFile::open(path)
        .with_context(|| format!("Failed to open FITS file: {}", path.display()))?;
    crate::logging::log("DEBUG", &format!("FITS header open OK: {}", path.display()));

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

/// Extract full XISF header as text
pub fn extract_xisf_header(path: &Path) -> Result<String> {
    use quick_xml::events::Event;
    use quick_xml::Reader;
    use std::fs::File;
    use std::io::{BufReader, Read};

    crate::logging::log("DEBUG", &format!("Opening XISF for header: {}", path.display()));
    // Read the first 1MB which should contain the XML header
    let file = File::open(path)
        .with_context(|| format!("Failed to open XISF file: {}", path.display()))?;
    let mut buf_reader = BufReader::new(file);

    let max_header_size = 1024 * 1024; // 1MB
    let mut content = vec![0u8; max_header_size];
    let bytes_read = buf_reader.read(&mut content)?;
    content.truncate(bytes_read);

    // Find the XML section
    let xml_start = content.windows(5)
        .position(|w| w == b"<?xml")
        .ok_or_else(|| anyhow::anyhow!("No XML header found in XISF file"))?;

    let xml_end = content.windows(7)
        .skip(xml_start)
        .position(|w| w == b"</xisf>")
        .ok_or_else(|| anyhow::anyhow!("No closing </xisf> tag found"))?;
    let xml_end = xml_start + xml_end + 7;

    // Extract XML content
    let xml_content = &content[xml_start..xml_end];
    let xml_str = String::from_utf8_lossy(xml_content);

    // Parse XML to extract FITSKeyword elements
    let mut reader = Reader::from_str(&xml_str);
    reader.config_mut().trim_text(true);

    let mut header_text = String::new();
    header_text.push_str("XISF FITS Keywords:\n");
    header_text.push_str("==================\n\n");

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(ref e)) if e.name().as_ref() == b"FITSKeyword" => {
                let mut name = String::new();
                let mut value = String::new();

                for attr in e.attributes() {
                    if let Ok(attr) = attr {
                        match attr.key.as_ref() {
                            b"name" => {
                                name = String::from_utf8_lossy(&attr.value).to_string();
                            }
                            b"value" => {
                                value = String::from_utf8_lossy(&attr.value).to_string();
                                // Remove quotes if present
                                if value.starts_with('\'') && value.ends_with('\'') {
                                    value = value[1..value.len()-1].to_string();
                                }
                            }
                            _ => {}
                        }
                    }
                }

                if !name.is_empty() {
                    header_text.push_str(&format!("{} = {}\n", name, value));
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                println!("Error parsing XISF XML: {}", e);
                break;
            }
            _ => {}
        }
        buf.clear();
    }

    // If header is still minimal, record that we tried
    if header_text.lines().count() <= 3 {
        header_text = format!("Header extracted from: {}\n(No FITS keywords found in XISF)", path.display());
    }

    Ok(header_text)
}

/// Parse FITS file metadata
pub fn parse_fits(path: &Path, file_id: i64) -> Result<Frame> {
    crate::logging::log("DEBUG", &format!("Opening FITS for parsing: {}", path.display()));

    let mut fitsfile = FitsFile::open(path)
        .with_context(|| format!("Failed to open FITS file: {}", path.display()))?;
    crate::logging::log("DEBUG", &format!("FITS parse open OK: {}", path.display()));

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
    let swcreate = read_keyword_string(&mut fitsfile, &hdu, "SWCREATE").ok();

    // Bayer pattern for OSC (one-shot color) cameras
    let bayerpat = read_keyword_string(&mut fitsfile, &hdu, "BAYERPAT").ok();

    // Pixel size
    let xpixsz = read_keyword_f64(&mut fitsfile, &hdu, "XPIXSZ").ok();
    let ypixsz = read_keyword_f64(&mut fitsfile, &hdu, "YPIXSZ").ok();

    // Image dimensions
    let naxis1 = read_keyword_i32(&mut fitsfile, &hdu, "NAXIS1").ok();
    let naxis2 = read_keyword_i32(&mut fitsfile, &hdu, "NAXIS2").ok();

    // Astronomical coordinates
    // Read raw values first
    let ra_raw = read_keyword_f64(&mut fitsfile, &hdu, "RA").ok();
    let dec_raw = read_keyword_f64(&mut fitsfile, &hdu, "DEC").ok();
    let objctra = read_keyword_string(&mut fitsfile, &hdu, "OBJCTRA").ok();
    let objctdec = read_keyword_string(&mut fitsfile, &hdu, "OBJCTDEC").ok();

    // Apply unit detection and validation
    // Pass objctra for verification (handles RA=0 and [0,24) ambiguity correctly)
    let ra = ra_raw.map(|r| normalize_ra_from_fits(r, dec_raw, objctra.as_deref()));
    let dec = dec_raw.and_then(|d| validate_dec(d).ok());

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

    // Determine if this is a master file
    // Priority 1: Check IMAGETYP keyword for "Master" prefix
    let is_master = imagetyp.as_ref().map(|t| t.is_master()).unwrap_or(false);

    // Priority 2: Check filename if IMAGETYP doesn't indicate master
    let filename_is_master = if !is_master {
        let filename = path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let filename_lower = filename.to_lowercase();
        filename_lower.contains("master") ||
        filename_lower.contains("_calibrated_") ||
        filename_lower.contains("-calibrated-")
    } else {
        false
    };

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
        is_master: is_master || filename_is_master,
        gain,
        offset,
        binning,
        xbinning,
        ybinning,
        ccd_temp,
        set_temp,
        focallen,
        xpixsz,
        ypixsz,
        naxis1,
        naxis2,
        ra,
        dec,
        sitelat,
        lat_obs,
        sitelong,
        long_obs,
        objctra,
        objctdec,
        override_: false,
        swcreate,
        bayerpat,
    })
}

/// Parse XISF file metadata
pub fn parse_xisf(path: &Path, file_id: i64) -> Result<Frame> {
    use quick_xml::events::Event;
    use quick_xml::Reader;
    use std::collections::HashMap;
    use std::fs::File;
    use std::io::{BufReader, Read};

    crate::logging::log("DEBUG", &format!("Opening XISF for parsing: {}", path.display()));

    // Read the file
    let file = File::open(path)
        .with_context(|| format!("Failed to open XISF file: {}", path.display()))?;
    let mut buf_reader = BufReader::new(file);

    // XISF format: binary header + XML header + image data
    // Read only the first 1MB which should contain the XML header
    // (XML headers are typically much smaller, but 1MB gives us safety margin)
    let max_header_size = 1024 * 1024; // 1MB
    let mut content = vec![0u8; max_header_size];
    let bytes_read = buf_reader.read(&mut content)?;
    content.truncate(bytes_read);

    // Find the XML section - it starts after "<?xml"
    let xml_start = content.windows(5)
        .position(|w| w == b"<?xml")
        .ok_or_else(|| anyhow::anyhow!("No XML header found in XISF file"))?;

    // Find the end of XML - look for </xisf>
    let xml_end = content.windows(7)
        .skip(xml_start)
        .position(|w| w == b"</xisf>")
        .ok_or_else(|| anyhow::anyhow!("No closing </xisf> tag found in first {}KB", max_header_size / 1024))?;
    let xml_end = xml_start + xml_end + 7; // +7 for the length of "</xisf>"

    // Extract XML content
    let xml_content = &content[xml_start..xml_end];
    let xml_str = String::from_utf8_lossy(xml_content);

    // Parse XML to extract FITSKeyword elements
    let mut reader = Reader::from_str(&xml_str);
    reader.config_mut().trim_text(true);

    let mut fits_keywords: HashMap<String, String> = HashMap::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(ref e)) if e.name().as_ref() == b"FITSKeyword" => {
                let mut name = String::new();
                let mut value = String::new();

                for attr in e.attributes() {
                    if let Ok(attr) = attr {
                        match attr.key.as_ref() {
                            b"name" => {
                                name = String::from_utf8_lossy(&attr.value).to_string();
                            }
                            b"value" => {
                                value = String::from_utf8_lossy(&attr.value).to_string();
                                // Remove quotes if present
                                if value.starts_with('\'') && value.ends_with('\'') {
                                    value = value[1..value.len()-1].to_string();
                                }
                            }
                            _ => {}
                        }
                    }
                }

                if !name.is_empty() {
                    fits_keywords.insert(name, value);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                println!("Error parsing XISF XML at position {}: {}", reader.buffer_position(), e);
                break;
            }
            _ => {}
        }
        buf.clear();
    }

    println!("  Found {} FITS keywords in XISF", fits_keywords.len());

    // Extract metadata from FITS keywords
    let object = fits_keywords.get("OBJECT").cloned();
    let telescop = fits_keywords.get("TELESCOP").cloned();
    let instrume = fits_keywords.get("INSTRUME").cloned();
    let filter = fits_keywords.get("FILTER").cloned();
    let imagetyp_str = fits_keywords.get("IMAGETYP").cloned();

    let exptime = fits_keywords.get("EXPTIME")
        .and_then(|s| s.parse::<f64>().ok());
    let gain = fits_keywords.get("GAIN")
        .and_then(|s| s.parse::<f64>().ok());
    let offset = fits_keywords.get("OFFSET")
        .and_then(|s| s.parse::<f64>().ok());
    let xbinning = fits_keywords.get("XBINNING")
        .and_then(|s| s.parse::<i32>().ok());
    let ybinning = fits_keywords.get("YBINNING")
        .and_then(|s| s.parse::<i32>().ok());
    let ccd_temp = fits_keywords.get("CCD-TEMP")
        .and_then(|s| s.parse::<f64>().ok());
    let set_temp = fits_keywords.get("SET-TEMP")
        .and_then(|s| s.parse::<f64>().ok());
    let focallen = fits_keywords.get("FOCALLEN")
        .and_then(|s| s.parse::<f64>().ok());
    let swcreate = fits_keywords.get("SWCREATE").cloned();
    let bayerpat = fits_keywords.get("BAYERPAT").cloned();
    let xpixsz = fits_keywords.get("XPIXSZ")
        .and_then(|s| s.parse::<f64>().ok());
    let ypixsz = fits_keywords.get("YPIXSZ")
        .and_then(|s| s.parse::<f64>().ok());

    // Image dimensions
    let naxis1 = fits_keywords.get("NAXIS1")
        .and_then(|s| s.parse::<i32>().ok());
    let naxis2 = fits_keywords.get("NAXIS2")
        .and_then(|s| s.parse::<i32>().ok());

    // Astronomical coordinates
    // Read raw values first
    let ra_raw = fits_keywords.get("RA")
        .and_then(|s| s.parse::<f64>().ok());
    let dec_raw = fits_keywords.get("DEC")
        .and_then(|s| s.parse::<f64>().ok());
    let objctra = fits_keywords.get("OBJCTRA").cloned();
    let objctdec = fits_keywords.get("OBJCTDEC").cloned();

    // Apply unit detection and validation
    // Pass objctra for verification (handles RA=0 and [0,24) ambiguity correctly)
    let ra = ra_raw.map(|r| normalize_ra_from_fits(r, dec_raw, objctra.as_deref()));
    let dec = dec_raw.and_then(|d| validate_dec(d).ok());

    // Observatory location
    let sitelat = fits_keywords.get("SITELAT")
        .and_then(|s| s.parse::<f64>().ok());
    let lat_obs = fits_keywords.get("LAT-OBS")
        .and_then(|s| s.parse::<f64>().ok());
    let sitelong = fits_keywords.get("SITELONG")
        .and_then(|s| s.parse::<f64>().ok());
    let long_obs = fits_keywords.get("LONG-OBS")
        .and_then(|s| s.parse::<f64>().ok());

    // Parse DATE-OBS
    let date_obs = fits_keywords.get("DATE-OBS")
        .and_then(|date_str| {
            let time_obs = fits_keywords.get("TIME-OBS").map(|s| s.as_str());
            match parse_date_obs(date_str, time_obs) {
                Ok(dt) => {
                    println!("  Parsed DATE-OBS successfully: {}", dt.to_rfc3339());
                    Some(dt)
                }
                Err(e) => {
                    println!("  Failed to parse DATE-OBS '{}': {}", date_str, e);
                    None
                }
            }
        });

    // Parse IMAGETYP
    let imagetyp = imagetyp_str.as_ref().and_then(|s| ImageType::from_str(s));

    // Determine if this is a master file
    let is_master = imagetyp.as_ref().map(|t| t.is_master()).unwrap_or(false);

    // Check filename if IMAGETYP doesn't indicate master
    let filename_is_master = if !is_master {
        let filename = path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let filename_lower = filename.to_lowercase();
        filename_lower.contains("master") ||
        filename_lower.contains("_calibrated_") ||
        filename_lower.contains("-calibrated-")
    } else {
        false
    };

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
        is_master: is_master || filename_is_master,
        gain,
        offset,
        binning,
        xbinning,
        ybinning,
        ccd_temp,
        set_temp,
        focallen,
        xpixsz,
        ypixsz,
        naxis1,
        naxis2,
        ra,
        dec,
        sitelat,
        lat_obs,
        sitelong,
        long_obs,
        objctra,
        objctdec,
        override_: false,
        swcreate,
        bayerpat,
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
    // Use i64 to work around fitsio bug: i32 uses fits_read_key_log (boolean)
    // instead of fits_read_key_lng (integer), causing values like 2 to become 1
    let value: i64 = hdu.read_key(fitsfile, key)?;
    Ok(value as i32)
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

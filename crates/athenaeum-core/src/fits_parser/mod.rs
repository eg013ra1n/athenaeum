// FITS/XISF metadata parser module

pub mod calibrated_light;
pub(crate) mod fits_header_reader;
pub mod stored_header;

use crate::models::{Frame, ImageType};
use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDateTime, Utc};
pub use calibrated_light::{calibrated_light_identity, CalibratedIdentity, MasterRef};
pub use fits_header_reader::FitsHeader; // Task 14: reachable for round-trip tests (fits_header_reader itself stays pub(crate))
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

/// Read the XISF XML header bytes from a file.
/// Parses the 16-byte binary header (8-byte signature + 4-byte header length + 4 reserved)
/// to determine exactly how many bytes to read.
fn read_xisf_header(path: &Path) -> Result<Vec<u8>> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("Failed to open XISF file: {}", path.display()))?;
    let mut buf_reader = BufReader::new(file);

    let mut sig = [0u8; 8];
    buf_reader.read_exact(&mut sig)?;
    if &sig != b"XISF0100" {
        anyhow::bail!("Not a valid XISF file: bad signature");
    }

    let mut len_bytes = [0u8; 4];
    buf_reader.read_exact(&mut len_bytes)?;
    let header_length = u32::from_le_bytes(len_bytes) as usize;

    // Safety cap at 64MB
    if header_length > 64 * 1024 * 1024 {
        anyhow::bail!("XISF header too large: {} bytes", header_length);
    }

    // Skip 4 reserved bytes, then read header_length bytes
    buf_reader.seek(SeekFrom::Current(4))?;
    let mut content = vec![0u8; header_length];
    buf_reader.read_exact(&mut content)?;
    Ok(content)
}

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
                tracing::debug!(ra, "RA already in degrees (verified against OBJCTRA)");
                return crate::coordinates::normalize_ra(ra);
            }

            // If numeric RA * 15 matches OBJCTRA (within 0.1°), it's in hours
            if diff_as_hours < 0.1 {
                tracing::debug!(ra_hours = ra, ra_degrees = ra * 15.0, "RA detected in hours (verified against OBJCTRA)");
                return crate::coordinates::normalize_ra(ra * 15.0);
            }

            // Neither match well - use OBJCTRA as ground truth
            tracing::warn!(ra, ra_from_objctra, "RA does not match OBJCTRA; using OBJCTRA value");
            return ra_from_objctra;
        }
    }

    // No OBJCTRA available, use heuristics
    if let Some(d) = dec {
        if d >= -90.0 && d <= 90.0 {
            // Valid DEC suggests these are coordinates, assume hours
            tracing::warn!(ra, ra_degrees = ra * 15.0, "RA ambiguous with no OBJCTRA; assuming hours based on valid DEC");
            return crate::coordinates::normalize_ra(ra * 15.0);
        }
    }

    // No context available, default to hours (astronomical convention)
    tracing::warn!(ra, "RA ambiguous with no verification available; assuming hours");
    crate::coordinates::normalize_ra(ra * 15.0)
}

/// Validates and normalizes DEC to [-90, 90] range
fn validate_dec(dec: f64) -> Result<f64, String> {
    if dec < -90.0 || dec > 90.0 {
        // Clamp to valid range and warn
        let clamped = crate::coordinates::normalize_dec(dec);
        tracing::warn!(dec, clamped, "DEC out of range; clamped");
        Ok(clamped)
    } else {
        Ok(dec)
    }
}

/// Extract full FITS header as text (all raw cards, not just a keyword whitelist)
pub fn extract_fits_header(path: &Path) -> Result<String> {
    tracing::debug!(path = %path.display(), "reading FITS header");
    let header = FitsHeader::from_path(path)?;
    Ok(header.to_header_text())
}

/// Parse FITS file and return both Frame metadata and raw header text in a single read.
pub fn parse_fits_with_header(path: &Path, file_id: i64) -> Result<(Frame, String)> {
    tracing::debug!(path = %path.display(), "parsing FITS (combined)");
    let header = FitsHeader::from_path(path)?;
    let header_text = header.to_header_text();
    let frame = build_frame_from_header(&header, file_id, path)?;
    Ok((frame, header_text))
}

/// Extract full XISF header as text
pub fn extract_xisf_header(path: &Path) -> Result<String> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    tracing::debug!(path = %path.display(), "opening XISF for header");
    let content = read_xisf_header(path)?;

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
    // Permissive dangling-`&` retained deliberately (0.36-era behavior; XISF
    // headers are machine-written but a malformed one should still render —
    // cycle decision 2026-07-29).
    reader.config_mut().allow_dangling_amp = true;

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
                tracing::error!(
                    path = %path.display(),
                    position = reader.buffer_position(),
                    error = %e,
                    "error parsing XISF XML header; using partial result"
                );
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

/// Parse FITS file metadata (single-result variant; prefer parse_fits_with_header for scanning)
#[allow(dead_code)]
pub fn parse_fits(path: &Path, file_id: i64) -> Result<Frame> {
    tracing::debug!(path = %path.display(), "parsing FITS");
    let header = FitsHeader::from_path(path)?;
    build_frame_from_header(&header, file_id, path)
}

/// Build a Frame from an already-parsed FitsHeader.
pub(crate) fn build_frame_from_header(header: &FitsHeader, file_id: i64, path: &Path) -> Result<Frame> {
    // Extract standard FITS keywords
    let object = header.get_str("OBJECT");
    let date_obs_str = header.get_str("DATE-OBS");
    let time_obs = header.get_str("TIME-OBS");

    tracing::debug!(
        path = %path.display(),
        file_id,
        date_obs = ?date_obs_str,
        time_obs = ?time_obs,
        "raw DATE-OBS/TIME-OBS from FITS header"
    );
    let telescop = header.get_str("TELESCOP");
    let instrume = header.get_str("INSTRUME");
    let exptime = header.get_f64("EXPTIME").or_else(|| header.get_f64("EXPOSURE"));
    let filter = header.get_str("FILTER");
    let imagetyp_str = header.get_str("IMAGETYP");

    // Additional metadata
    let gain = header.get_f64("GAIN");
    let offset = header.get_f64("OFFSET");
    let xbinning = header.get_i32("XBINNING");
    let ybinning = header.get_i32("YBINNING");
    let ccd_temp = header.get_f64("CCD-TEMP");
    let set_temp = header.get_f64("SET-TEMP");
    let focallen = header.get_f64("FOCALLEN");
    let swcreate = header.get_str("SWCREATE");

    // Bayer pattern for OSC (one-shot color) cameras, plus the CFA phase
    // offsets and row order that go with it. All three stay None when the
    // header is silent — a fabricated offset misaligns the CFA grid.
    // `get_i32` is reused for the numeric reads (it also tolerates the "1.0"
    // spelling some writers emit); the catalog stores them as i64.
    let bayerpat = header.get_str("BAYERPAT");
    let xbayroff = header.get_i32("XBAYROFF").map(i64::from);
    let ybayroff = header.get_i32("YBAYROFF").map(i64::from);
    let roworder = header.get_str("ROWORDER");

    // Pixel size (with PIXSIZE1/PIXSIZE2 fallback)
    let xpixsz = header.get_f64("XPIXSZ")
        .or_else(|| header.get_f64("PIXSIZE1"));
    let ypixsz = header.get_f64("YPIXSZ")
        .or_else(|| header.get_f64("PIXSIZE2"));

    // Image dimensions
    let naxis1 = header.get_i32("NAXIS1");
    let naxis2 = header.get_i32("NAXIS2");

    // Astronomical coordinates
    // Read raw values first
    let ra_raw = header.get_f64("RA");
    let dec_raw = header.get_f64("DEC");
    let objctra = header.get_str("OBJCTRA");
    let objctdec = header.get_str("OBJCTDEC");

    // Apply unit detection and validation
    // Pass objctra for verification (handles RA=0 and [0,24) ambiguity correctly)
    let ra = ra_raw.map(|r| normalize_ra_from_fits(r, dec_raw, objctra.as_deref()));
    let dec = dec_raw.and_then(|d| validate_dec(d).ok());

    // Fallback: parse OBJCTRA/OBJCTDEC if numeric RA/DEC missing
    let ra = ra.or_else(|| {
        objctra.as_deref().and_then(|s| crate::coordinates::parse_ra_sexagesimal(s).ok())
    });
    let dec = dec.or_else(|| {
        objctdec.as_deref().and_then(|s| crate::coordinates::parse_dec_sexagesimal(s).ok())
    });

    // WCS fallback: CRVAL1/CRVAL2 when CTYPE indicates RA/DEC
    let ra = ra.or_else(|| {
        let ctype1 = header.get_str("CTYPE1")?;
        if ctype1.to_uppercase().starts_with("RA") {
            header.get_f64("CRVAL1").map(|r| crate::coordinates::normalize_ra(r))
        } else {
            None
        }
    });
    let dec = dec.or_else(|| {
        let ctype2 = header.get_str("CTYPE2")?;
        if ctype2.to_uppercase().starts_with("DEC") {
            header.get_f64("CRVAL2").and_then(|d| validate_dec(d).ok())
        } else {
            None
        }
    });

    // Image rotation / position angle — fallback priority chain.
    // Sky-frame keywords first; mechanical-rotator-angle keywords last.
    // See the matching XISF-path comment below for the full reasoning and
    // the 2026-05 demotion of ROTATANG. Both paths must stay in lock-step.
    //
    // 1. CROTA2    — FITS WCS standard (sky frame)
    // 2. CD matrix — atan2(-CD1_2, CD2_2) (sky frame)
    // 3. CROTA1    — FITS WCS alt-axis rotation (sky frame)
    // 4. POSANGLE  — Sky position angle (sky frame)
    // 5. PA        — Generic position angle (sky frame)
    // 6. OBJCTROT  — Planned target rotation (NINA/PixInsight, sky frame)
    // 7. ROTATANG  — Mechanical rotator angle (instrument frame, fallback)
    // 8. APTS_ROT  — APT mechanical rotator angle (instrument frame, fallback)
    let rotation = header.get_f64("CROTA2")
        .or_else(|| {
            match (header.get_f64("CD1_2"), header.get_f64("CD2_2")) {
                (Some(cd1_2), Some(cd2_2)) => Some((-cd1_2).atan2(cd2_2).to_degrees()),
                _ => None,
            }
        })
        .or_else(|| header.get_f64("CROTA1"))
        .or_else(|| header.get_f64("POSANGLE"))
        .or_else(|| header.get_f64("PA"))
        .or_else(|| header.get_f64("OBJCTROT"))
        .or_else(|| header.get_f64("ROTATANG"))
        .or_else(|| header.get_f64("APTS_ROT"));

    // Observatory location
    let sitelat = header.get_f64("SITELAT");
    let lat_obs = header.get_f64("LAT-OBS");
    let sitelong = header.get_f64("SITELONG");
    let long_obs = header.get_f64("LONG-OBS");

    // Parse DATE-OBS
    let date_obs = match (date_obs_str.clone(), time_obs.clone()) {
        (Some(date), time) => {
            match parse_date_obs(&date, time.as_deref()) {
                Ok(dt) => {
                    tracing::debug!(path = %path.display(), file_id, date_obs = %dt.to_rfc3339(), "parsed DATE-OBS");
                    Some(dt)
                },
                Err(e) => {
                    tracing::warn!(path = %path.display(), file_id, error = %e, "failed to parse DATE-OBS; leaving unset");
                    None
                }
            }
        },
        _ => {
            tracing::warn!(path = %path.display(), file_id, "no DATE-OBS found in FITS header");
            None
        }
    };

    // Parse IMAGETYP
    let imagetyp = imagetyp_str.and_then(|s| ImageType::from_str(&s))
        .or_else(|| header.get_str("FRAME").and_then(|s| ImageType::from_str(&s)));

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
        xbayroff,
        ybayroff,
        roworder,
        rotation,
        uuid: None,
        updated_at: None,
    })
}

/// Parse XISF file metadata
pub fn parse_xisf(path: &Path, file_id: i64) -> Result<Frame> {
    use quick_xml::events::Event;
    use quick_xml::Reader;
    use std::collections::HashMap;

    tracing::debug!(path = %path.display(), "opening XISF for parsing");

    let content = read_xisf_header(path)?;

    // Find the XML section - it starts after "<?xml"
    let xml_start = content.windows(5)
        .position(|w| w == b"<?xml")
        .ok_or_else(|| anyhow::anyhow!("No XML header found in XISF file"))?;

    // Find the end of XML - look for </xisf>
    let xml_end = content.windows(7)
        .skip(xml_start)
        .position(|w| w == b"</xisf>")
        .ok_or_else(|| anyhow::anyhow!("No closing </xisf> tag found"))?;
    let xml_end = xml_start + xml_end + 7; // +7 for the length of "</xisf>"

    // Extract XML content
    let xml_content = &content[xml_start..xml_end];
    let xml_str = String::from_utf8_lossy(xml_content);

    // Parse XML to extract FITSKeyword elements
    let mut reader = Reader::from_str(&xml_str);
    reader.config_mut().trim_text(true);
    // Permissive dangling-`&` retained deliberately (0.36-era behavior; XISF
    // headers are machine-written but a malformed one should still render —
    // cycle decision 2026-07-29).
    reader.config_mut().allow_dangling_amp = true;

    let mut fits_keywords: HashMap<String, String> = HashMap::new();
    let mut xisf_geometry: Option<(i32, i32)> = None;
    let mut xisf_properties: HashMap<String, String> = HashMap::new();
    let mut xisf_cfa_pattern: Option<String> = None;
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
            // Parse <Image geometry="w:h:c"> for NAXIS1/NAXIS2 fallback
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e))
                if e.name().as_ref() == b"Image" =>
            {
                for attr in e.attributes().flatten() {
                    if attr.key.as_ref() == b"geometry" {
                        let geom = String::from_utf8_lossy(&attr.value);
                        let parts: Vec<&str> = geom.split(':').collect();
                        if parts.len() >= 2 {
                            if let (Ok(w), Ok(h)) = (parts[0].parse::<i32>(), parts[1].parse::<i32>()) {
                                xisf_geometry = Some((w, h));
                            }
                        }
                    }
                }
            }
            // Parse <Property> elements for native XISF metadata
            Ok(Event::Empty(ref e)) if e.name().as_ref() == b"Property" => {
                let mut prop_id = String::new();
                let mut prop_value = String::new();
                for attr in e.attributes().flatten() {
                    match attr.key.as_ref() {
                        b"id" => prop_id = String::from_utf8_lossy(&attr.value).to_string(),
                        b"value" => prop_value = String::from_utf8_lossy(&attr.value).to_string(),
                        _ => {}
                    }
                }
                if !prop_id.is_empty() && !prop_value.is_empty() {
                    xisf_properties.insert(prop_id, prop_value);
                }
            }
            // Native XISF CFA declaration: <ColorFilterArray pattern="RGGB"
            // width="2" height="2" name="RGGB"/>. Taken raw here; validated and
            // adopted below, only when no BAYERPAT keyword is present.
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e))
                if e.name().as_ref() == b"ColorFilterArray" =>
            {
                for attr in e.attributes().flatten() {
                    if attr.key.as_ref() == b"pattern" {
                        xisf_cfa_pattern = Some(String::from_utf8_lossy(&attr.value).to_string());
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                tracing::error!(
                    path = %path.display(),
                    file_id,
                    position = reader.buffer_position(),
                    error = %e,
                    "error parsing XISF XML; using partial result"
                );
                break;
            }
            _ => {}
        }
        buf.clear();
    }

    tracing::debug!(path = %path.display(), file_id, count = fits_keywords.len(), "extracted FITS keywords from XISF");

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
        .and_then(|s| s.parse::<i32>().ok())
        .or_else(|| xisf_properties.get("Instrument:Camera:XBinning")
            .and_then(|s| s.parse::<i32>().ok()));
    let ybinning = fits_keywords.get("YBINNING")
        .and_then(|s| s.parse::<i32>().ok())
        .or_else(|| xisf_properties.get("Instrument:Camera:YBinning")
            .and_then(|s| s.parse::<i32>().ok()));
    let ccd_temp = fits_keywords.get("CCD-TEMP")
        .and_then(|s| s.parse::<f64>().ok());
    let set_temp = fits_keywords.get("SET-TEMP")
        .and_then(|s| s.parse::<f64>().ok());
    let focallen = fits_keywords.get("FOCALLEN")
        .and_then(|s| s.parse::<f64>().ok());
    let swcreate = fits_keywords.get("SWCREATE").cloned();
    // A blank keyword is not a declaration — same rule as ROWORDER below and as
    // the FITS path's `get_str`. `<FITSKeyword name="BAYERPAT" value=""/>` used
    // to land as `Some("")` in `frames.bayerpat`, where it outvoted real
    // patterns in the master-header consensus and blocked the element fallback
    // right below from ever running.
    let bayerpat = fits_keywords.get("BAYERPAT").cloned().filter(|s| !s.trim().is_empty());
    // Fall back to the native XISF <ColorFilterArray> element: a writer that
    // emits the XISF 1.0 element and no BAYERPAT keyword is declaring a mosaic
    // just as plainly, and treating it as MONO poisons everything downstream.
    // An explicit BAYERPAT keyword keeps precedence. The element carries no
    // phase offsets, so xbayroff/ybayroff stay None below — an assumed phase
    // misaligns the CFA grid, and absent beats fabricated.
    let bayerpat = bayerpat.or_else(|| {
        let raw = xisf_cfa_pattern.as_ref()?;
        let pattern = raw.trim().to_uppercase();
        if !pattern.is_empty() && pattern.bytes().all(|b| matches!(b, b'R' | b'G' | b'B')) {
            tracing::debug!(path = %path.display(), file_id, pattern = %pattern, "adopted cfa pattern from xisf element");
            Some(pattern)
        } else {
            tracing::warn!(path = %path.display(), file_id, pattern = %raw, "unrecognized xisf cfa pattern");
            None
        }
    });
    // CFA phase offsets + row order — kept in lock-step with the FITS card
    // path above: the float fallback below is equivalent for realistic offset
    // spellings (`get_i32` routes its fallback through `parse_fits_f64`, which
    // additionally accepts Fortran D-notation — never seen on these cards).
    // Absent keywords stay None; 0 is a real phase, not a default.
    let parse_offset = |s: &String| -> Option<i64> {
        let s = s.trim();
        s.parse::<i64>().ok().or_else(|| s.parse::<f64>().ok().map(|f| f as i64))
    };
    let xbayroff = fits_keywords.get("XBAYROFF").and_then(parse_offset);
    let ybayroff = fits_keywords.get("YBAYROFF").and_then(parse_offset);
    let roworder = fits_keywords.get("ROWORDER")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let xpixsz = fits_keywords.get("XPIXSZ")
        .and_then(|s| s.parse::<f64>().ok())
        .or_else(|| fits_keywords.get("PIXSIZE1").and_then(|s| s.parse::<f64>().ok()))
        .or_else(|| xisf_properties.get("Instrument:Sensor:XPixelSize")
            .and_then(|s| s.parse::<f64>().ok()));
    let ypixsz = fits_keywords.get("YPIXSZ")
        .and_then(|s| s.parse::<f64>().ok())
        .or_else(|| fits_keywords.get("PIXSIZE2").and_then(|s| s.parse::<f64>().ok()))
        .or_else(|| xisf_properties.get("Instrument:Sensor:YPixelSize")
            .and_then(|s| s.parse::<f64>().ok()));

    // Image dimensions (with <Image geometry="w:h:c"> fallback)
    let naxis1 = fits_keywords.get("NAXIS1")
        .and_then(|s| s.parse::<i32>().ok())
        .or_else(|| xisf_geometry.map(|(w, _)| w));
    let naxis2 = fits_keywords.get("NAXIS2")
        .and_then(|s| s.parse::<i32>().ok())
        .or_else(|| xisf_geometry.map(|(_, h)| h));

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

    // Fallback: parse OBJCTRA/OBJCTDEC if numeric RA/DEC missing
    let ra = ra.or_else(|| {
        objctra.as_ref().and_then(|s| crate::coordinates::parse_ra_sexagesimal(s).ok())
    });
    let dec = dec.or_else(|| {
        objctdec.as_ref().and_then(|s| crate::coordinates::parse_dec_sexagesimal(s).ok())
    });

    // WCS fallback: CRVAL1/CRVAL2 when CTYPE indicates RA/DEC
    let ra = ra.or_else(|| {
        let ctype1 = fits_keywords.get("CTYPE1")?;
        if ctype1.to_uppercase().starts_with("RA") {
            fits_keywords.get("CRVAL1")
                .and_then(|s| s.parse::<f64>().ok())
                .map(|r| crate::coordinates::normalize_ra(r))
        } else {
            None
        }
    });
    let dec = dec.or_else(|| {
        let ctype2 = fits_keywords.get("CTYPE2")?;
        if ctype2.to_uppercase().starts_with("DEC") {
            fits_keywords.get("CRVAL2")
                .and_then(|s| s.parse::<f64>().ok())
                .and_then(|d| validate_dec(d).ok())
        } else {
            None
        }
    });

    // Image rotation / position angle — fallback priority chain.
    //
    // Sky-frame keywords (the rotation of the image on the sky, east of
    // north) come first. Mechanical-frame keywords (the raw rotator hardware
    // angle, which depends on the rotator's zero offset and only loosely
    // maps to sky orientation) come last. The 1-3 entries are post-solve
    // and authoritative; 4-6 are pre-solve sky-frame estimates from the
    // acquisition software; 7-8 are mechanical fallbacks of last resort.
    //
    // 1. CROTA2    — FITS WCS standard (sky frame, direct rotation N→E)
    // 2. CD matrix — atan2(-CD1_2, CD2_2) (sky frame, derived from WCS)
    // 3. CROTA1    — FITS WCS alt-axis rotation (sky frame)
    // 4. POSANGLE  — Mount/plate-solve software, sky position angle
    // 5. PA        — Generic position angle (sky frame, east of north)
    // 6. OBJCTROT  — Planned target rotation (NINA/PixInsight, sky frame)
    // 7. ROTATANG  — Mechanical rotator angle (instrument frame; only
    //                a meaningful sky angle when the rotator is zeroed
    //                to true sky east-of-north, which is rarely true).
    //                Demoted in 2026-05 — was 5th, but produced wrong
    //                rotations for users with rotator offsets, e.g. a
    //                NINA rig with OBJCTROT=185 / ROTATANG=208 (a 23°
    //                offset). The actual sky rotation from the WCS solve
    //                was 355°, matching neither pre-solve number — but
    //                OBJCTROT is at least in sky semantics and lets the
    //                user reason about framing.
    // 8. APTS_ROT  — APT mechanical rotator angle (same caveats as ROTATANG).
    let rotation = fits_keywords.get("CROTA2")
        .and_then(|s| s.parse::<f64>().ok())
        .or_else(|| {
            match (
                fits_keywords.get("CD1_2").and_then(|s| s.parse::<f64>().ok()),
                fits_keywords.get("CD2_2").and_then(|s| s.parse::<f64>().ok()),
            ) {
                (Some(cd1_2), Some(cd2_2)) => Some((-cd1_2).atan2(cd2_2).to_degrees()),
                _ => None,
            }
        })
        .or_else(|| fits_keywords.get("CROTA1").and_then(|s| s.parse::<f64>().ok()))
        .or_else(|| fits_keywords.get("POSANGLE").and_then(|s| s.parse::<f64>().ok()))
        .or_else(|| fits_keywords.get("PA").and_then(|s| s.parse::<f64>().ok()))
        .or_else(|| fits_keywords.get("OBJCTROT").and_then(|s| s.parse::<f64>().ok()))
        .or_else(|| fits_keywords.get("ROTATANG").and_then(|s| s.parse::<f64>().ok()))
        .or_else(|| fits_keywords.get("APTS_ROT").and_then(|s| s.parse::<f64>().ok()));

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
                    tracing::debug!(path = %path.display(), file_id, date_obs = %dt.to_rfc3339(), "parsed DATE-OBS");
                    Some(dt)
                }
                Err(e) => {
                    tracing::warn!(path = %path.display(), file_id, date_str = %date_str, error = %e, "failed to parse DATE-OBS; leaving unset");
                    None
                }
            }
        });

    // Parse IMAGETYP
    let imagetyp = imagetyp_str.as_ref().and_then(|s| ImageType::from_str(s))
        .or_else(|| fits_keywords.get("FRAME").and_then(|s| ImageType::from_str(s)));

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
        xbayroff,
        ybayroff,
        roworder,
        rotation,
        uuid: None,
        updated_at: None,
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

    #[test]
    fn test_exptime_falls_back_to_exposure() {
        // Build a FITS header block with EXPOSURE=120.0 but no EXPTIME
        const BLOCK_SIZE: usize = 2880;
        const CARD_SIZE: usize = 80;
        let mut block = vec![b' '; BLOCK_SIZE];

        fn write_card(block: &mut [u8], index: usize, card: &str) {
            let start = index * CARD_SIZE;
            let bytes = card.as_bytes();
            for (i, &b) in bytes.iter().enumerate() {
                if start + i < block.len() {
                    block[start + i] = b;
                }
            }
        }

        write_card(&mut block, 0, "SIMPLE  =                    T / Standard FITS");
        write_card(&mut block, 1, "BITPIX  =                   16 / bits per pixel");
        write_card(&mut block, 2, "NAXIS   =                    2 / number of axes");
        write_card(&mut block, 3, "OBJECT  = 'Test Object'        / Target name");
        write_card(&mut block, 4, "EXPOSURE=              120.000 / Exposure time");
        write_card(&mut block, 5, "FILTER  = 'R       '           / Filter name");
        write_card(&mut block, 6, "END                                                                             ");

        let header = FitsHeader::parse_header(&block).unwrap();
        let path = std::path::Path::new("/tmp/test.fits");
        let frame = build_frame_from_header(&header, 1, path).unwrap();
        assert_eq!(frame.exptime, Some(120.0));
    }

    #[test]
    fn test_exptime_takes_precedence_over_exposure() {
        // Build a FITS header block with both EXPTIME and EXPOSURE; EXPTIME should win
        const BLOCK_SIZE: usize = 2880;
        const CARD_SIZE: usize = 80;
        let mut block = vec![b' '; BLOCK_SIZE];

        fn write_card(block: &mut [u8], index: usize, card: &str) {
            let start = index * CARD_SIZE;
            let bytes = card.as_bytes();
            for (i, &b) in bytes.iter().enumerate() {
                if start + i < block.len() {
                    block[start + i] = b;
                }
            }
        }

        write_card(&mut block, 0, "SIMPLE  =                    T / Standard FITS");
        write_card(&mut block, 1, "BITPIX  =                   16 / bits per pixel");
        write_card(&mut block, 2, "NAXIS   =                    2 / number of axes");
        write_card(&mut block, 3, "OBJECT  = 'Test Object'        / Target name");
        write_card(&mut block, 4, "EXPTIME =               60.000 / Exposure time");
        write_card(&mut block, 5, "EXPOSURE=              120.000 / Alternative exposure");
        write_card(&mut block, 6, "FILTER  = 'R       '           / Filter name");
        write_card(&mut block, 7, "END                                                                             ");

        let header = FitsHeader::parse_header(&block).unwrap();
        let path = std::path::Path::new("/tmp/test.fits");
        let frame = build_frame_from_header(&header, 1, path).unwrap();
        assert_eq!(frame.exptime, Some(60.0));
    }

    /// Build a one-block FITS header out of already-formatted 80-column cards.
    /// SIMPLE/BITPIX/NAXIS are prepended and END appended, so callers only
    /// pass the cards under test.
    fn header_with_cards(cards: &[&str]) -> FitsHeader {
        const BLOCK_SIZE: usize = 2880;
        const CARD_SIZE: usize = 80;
        let mut block = vec![b' '; BLOCK_SIZE];

        let mut write_card = |index: usize, card: &str| {
            let start = index * CARD_SIZE;
            for (i, &b) in card.as_bytes().iter().enumerate() {
                if start + i < block.len() {
                    block[start + i] = b;
                }
            }
        };

        write_card(0, "SIMPLE  =                    T / Standard FITS");
        write_card(1, "BITPIX  =                   16 / bits per pixel");
        write_card(2, "NAXIS   =                    2 / number of axes");
        for (i, card) in cards.iter().enumerate() {
            write_card(3 + i, card);
        }
        write_card(3 + cards.len(), "END");

        FitsHeader::parse_header(&block).unwrap()
    }

    /// The CFA phase offsets and the row order must reach the Frame from the
    /// FITS card path — masters and light calibration need the source frame's
    /// real phase, not an assumed one.
    #[test]
    fn bayer_offsets_and_roworder_are_parsed() {
        let header = header_with_cards(&[
            "BAYERPAT= 'RGGB    '           / Bayer color pattern",
            "XBAYROFF=                    1 / Bayer X offset",
            "YBAYROFF=                    0 / Bayer Y offset",
            "ROWORDER= 'TOP-DOWN'           / image row order",
        ]);
        let path = std::path::Path::new("/tmp/test.fits");
        let frame = build_frame_from_header(&header, 1, path).unwrap();

        assert_eq!(frame.bayerpat.as_deref(), Some("RGGB"));
        assert_eq!(frame.xbayroff, Some(1));
        assert_eq!(frame.ybayroff, Some(0));
        assert_eq!(frame.roworder.as_deref(), Some("TOP-DOWN"));
    }

    /// A header that declares a Bayer pattern but no offsets leaves them
    /// `None`. Defaulting them to 0 would be a fabricated CFA phase — the very
    /// bug this plumbing exists to make impossible.
    #[test]
    fn absent_bayer_offsets_stay_none() {
        let header = header_with_cards(&[
            "BAYERPAT= 'RGGB    '           / Bayer color pattern",
        ]);
        let path = std::path::Path::new("/tmp/test.fits");
        let frame = build_frame_from_header(&header, 1, path).unwrap();

        assert_eq!(frame.bayerpat.as_deref(), Some("RGGB"));
        assert_eq!(frame.xbayroff, None, "absent XBAYROFF must not become 0");
        assert_eq!(frame.ybayroff, None, "absent YBAYROFF must not become 0");
        assert_eq!(frame.roworder, None);
    }

    /// The riskiest cross-software variant: an integer written as a quoted
    /// FITS string. Dropping it would report "unknown phase" for a header that
    /// plainly declares one.
    #[test]
    fn quoted_int_bayer_offsets_are_parsed() {
        let header = header_with_cards(&[
            "BAYERPAT= 'RGGB    '",
            "XBAYROFF= '1       '",
            "YBAYROFF= '0       '",
        ]);
        let path = std::path::Path::new("/tmp/test.fits");
        let frame = build_frame_from_header(&header, 1, path).unwrap();
        assert_eq!(frame.xbayroff, Some(1));
        assert_eq!(frame.ybayroff, Some(0));

        let dir = tempfile::TempDir::new().unwrap();
        let xisf = dir.path().join("osc_quoted.xisf");
        write_xisf_with_keywords(
            &xisf,
            &[("BAYERPAT", "'RGGB'"), ("XBAYROFF", "'1'"), ("YBAYROFF", "'0'")],
        );
        let frame = parse_xisf(&xisf, 1).unwrap();
        assert_eq!(frame.xbayroff, Some(1));
        assert_eq!(frame.ybayroff, Some(0));
    }

    /// Some writers spell an integer-valued card as a float ("1.0"). Both
    /// parser paths must still land an offset rather than silently dropping it
    /// to "unknown".
    #[test]
    fn float_spelled_bayer_offsets_are_parsed() {
        let header = header_with_cards(&[
            "BAYERPAT= 'RGGB    '",
            "XBAYROFF=                  1.0",
            "YBAYROFF=                  0.0",
        ]);
        let path = std::path::Path::new("/tmp/test.fits");
        let frame = build_frame_from_header(&header, 1, path).unwrap();
        assert_eq!(frame.xbayroff, Some(1));
        assert_eq!(frame.ybayroff, Some(0));

        let dir = tempfile::TempDir::new().unwrap();
        let xisf = dir.path().join("osc_float.xisf");
        write_xisf_with_keywords(
            &xisf,
            &[("BAYERPAT", "'RGGB'"), ("XBAYROFF", "1.0"), ("YBAYROFF", "0.0")],
        );
        let frame = parse_xisf(&xisf, 1).unwrap();
        assert_eq!(frame.xbayroff, Some(1));
        assert_eq!(frame.ybayroff, Some(0));
    }

    /// Write a minimal XISF file (16-byte binary header + XML) carrying the
    /// given FITSKeyword name/value pairs. Values are passed through verbatim,
    /// so a caller can spell a string value with the XISF `'...'` quoting.
    fn write_xisf_with_keywords(path: &std::path::Path, keywords: &[(&str, &str)]) {
        let mut body = String::new();
        for (name, value) in keywords {
            body.push_str(&format!(
                "<FITSKeyword name=\"{}\" value=\"{}\" comment=\"\"/>\n",
                name, value
            ));
        }
        write_xisf_with_image_body(path, &body);
    }

    /// Same minimal XISF container, but the `<Image>` body is passed verbatim —
    /// the general form behind `write_xisf_with_keywords`, used by the CFA tests
    /// to place a native `<ColorFilterArray …/>` next to (or instead of) the
    /// FITSKeyword cards.
    fn write_xisf_with_image_body(path: &std::path::Path, body: &str) {
        let xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<xisf version=\"1.0\">\n<Image geometry=\"100:100:1\">\n{}</Image>\n</xisf>",
            body
        );

        let xml_bytes = xml.as_bytes();
        let mut out: Vec<u8> = Vec::with_capacity(16 + xml_bytes.len());
        out.extend_from_slice(b"XISF0100");
        out.extend_from_slice(&(xml_bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(&[0u8; 4]); // reserved
        out.extend_from_slice(xml_bytes);
        std::fs::write(path, out).unwrap();
    }

    /// Same contract on the XISF keyword path — the two parsers must stay in
    /// lock-step.
    #[test]
    fn xisf_bayer_offsets_and_roworder_are_parsed() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("osc.xisf");
        write_xisf_with_keywords(
            &path,
            &[
                ("BAYERPAT", "'RGGB'"),
                ("XBAYROFF", "1"),
                ("YBAYROFF", "0"),
                ("ROWORDER", "'TOP-DOWN'"),
            ],
        );

        let frame = parse_xisf(&path, 1).unwrap();
        assert_eq!(frame.bayerpat.as_deref(), Some("RGGB"));
        assert_eq!(frame.xbayroff, Some(1));
        assert_eq!(frame.ybayroff, Some(0));
        assert_eq!(frame.roworder.as_deref(), Some("TOP-DOWN"));
    }

    /// XISF absence case — same "never fabricate 0" rule as the FITS path.
    #[test]
    fn xisf_absent_bayer_offsets_stay_none() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("mono.xisf");
        write_xisf_with_keywords(&path, &[("OBJECT", "'M33'")]);

        let frame = parse_xisf(&path, 1).unwrap();
        assert_eq!(frame.xbayroff, None, "absent XBAYROFF must not become 0");
        assert_eq!(frame.ybayroff, None, "absent YBAYROFF must not become 0");
        assert_eq!(frame.roworder, None);
    }

    /// The native XISF CFA declaration. Files written with only
    /// `<ColorFilterArray …/>` and no BAYERPAT FITSKeyword are common, and
    /// without this they read as MONO end-to-end.
    #[test]
    fn xisf_colorfilterarray_pattern_is_read() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("cfa_only.xisf");
        write_xisf_with_image_body(
            &path,
            "<FITSKeyword name=\"OBJECT\" value=\"'M42'\" comment=\"\"/>\n\
             <ColorFilterArray pattern=\"RGGB\" width=\"2\" height=\"2\" name=\"RGGB\"/>\n",
        );

        let frame = parse_xisf(&path, 1).unwrap();
        assert_eq!(frame.bayerpat.as_deref(), Some("RGGB"));
        // The element carries no phase offsets — assuming (0,0) would misalign
        // the CFA grid just as surely as a wrong pattern would.
        assert_eq!(frame.xbayroff, None, "the CFA element declares no phase");
        assert_eq!(frame.ybayroff, None, "the CFA element declares no phase");
    }

    /// A BAYERPAT FITSKeyword is the explicit statement and outranks the
    /// element — the two disagreeing is a real (if rare) shape.
    #[test]
    fn xisf_bayerpat_keyword_outranks_colorfilterarray() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("cfa_both.xisf");
        write_xisf_with_image_body(
            &path,
            "<FITSKeyword name=\"BAYERPAT\" value=\"'BGGR'\" comment=\"\"/>\n\
             <ColorFilterArray pattern=\"RGGB\" width=\"2\" height=\"2\" name=\"RGGB\"/>\n",
        );

        let frame = parse_xisf(&path, 1).unwrap();
        assert_eq!(frame.bayerpat.as_deref(), Some("BGGR"));
    }

    /// A BLANK keyword is not a statement, so it must not outrank the element
    /// that is one. Writers do emit `value=""` cards, and the keyword used to
    /// win on presence alone — leaving a declared mosaic reading as mono.
    #[test]
    fn xisf_blank_bayerpat_keyword_defers_to_colorfilterarray() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("cfa_blank_kw.xisf");
        write_xisf_with_image_body(
            &path,
            "<FITSKeyword name=\"BAYERPAT\" value=\"\" comment=\"\"/>\n\
             <ColorFilterArray pattern=\"RGGB\" width=\"2\" height=\"2\" name=\"RGGB\"/>\n",
        );

        let frame = parse_xisf(&path, 1).unwrap();
        assert_eq!(frame.bayerpat.as_deref(), Some("RGGB"), "the element is the only declaration present");
    }

    /// The same blank with nothing to fall back to: `None`, never `Some("")`.
    /// A stored empty string is a value downstream — it voted in the
    /// master-header consensus and could beat a real pattern.
    #[test]
    fn xisf_blank_bayerpat_keyword_alone_stays_none() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("blank_kw.xisf");
        write_xisf_with_keywords(&path, &[("BAYERPAT", "'   '"), ("OBJECT", "'M33'")]);

        let frame = parse_xisf(&path, 1).unwrap();
        assert_eq!(frame.bayerpat, None, "a blank keyword declares nothing");
    }

    /// A pattern we cannot interpret is reported and dropped — a bogus mosaic
    /// declaration is worse than none, and must not panic the scan.
    #[test]
    fn xisf_unrecognized_colorfilterarray_pattern_is_dropped() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("cfa_garbage.xisf");
        write_xisf_with_image_body(
            &path,
            "<ColorFilterArray pattern=\"XYZW\" width=\"2\" height=\"2\" name=\"XYZW\"/>\n",
        );

        let frame = parse_xisf(&path, 1).unwrap();
        assert_eq!(frame.bayerpat, None);
    }

    /// Neither declaration present — the frame stays mono, no invented pattern.
    #[test]
    fn xisf_without_cfa_declaration_stays_mono() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("mono_nocfa.xisf");
        write_xisf_with_keywords(&path, &[("OBJECT", "'M33'")]);

        let frame = parse_xisf(&path, 1).unwrap();
        assert_eq!(frame.bayerpat, None);
    }
}

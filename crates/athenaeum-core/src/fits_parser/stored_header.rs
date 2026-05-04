//! Parse a header blob that's been stored in `fits_header.header` back into
//! a keyword map, then map known keys to the canonical fields the catalog
//! cares about.
//!
//! The scanner stores two formats in the same TEXT column:
//!   - **FITS**: 80-byte cards joined with `\n` (see `FitsHeader::to_header_text`).
//!   - **XISF**: the raw `<?xml...</xisf>` block.
//!
//! Re-parsing here lets the UI show "what the file looked like at the most
//! recent scan" so a user can compare against their custom edits and revert
//! individual fields if they want to.
//!
//! Note: this is intentionally line-based (rather than byte-stream) so the
//! stored text — which has cards joined by newlines, not contiguous 2880-byte
//! blocks — round-trips cleanly without padding/realigning.

use crate::models::{FileFormat, ImageType};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Subset of FITS/XISF keywords the metadata pane lets users edit. All values
/// are normalised to canonical forms so the UI can compare against current
/// catalog state without a second normalisation pass.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameOriginalSnapshot {
    pub frame_id: i64,
    pub object: Option<String>,
    pub filter: Option<String>,
    /// IMAGETYP normalised through `ImageType::from_str` (e.g. "MasterDark").
    pub imagetyp: Option<String>,
    pub instrume: Option<String>,
    pub telescop: Option<String>,
    pub focallen: Option<f64>,
    pub gain: Option<f64>,
    pub offset: Option<f64>,
    /// Reconstructed "AxB" string from XBINNING/YBINNING (or the raw BINNING
    /// keyword when present). Mirrors what `bulk_update_frame_metadata` writes.
    pub binning: Option<String>,
    pub exptime: Option<f64>,
    pub ccd_temp: Option<f64>,
    /// DATE-OBS as it appears in the header (no timezone normalisation —
    /// the UI shows it verbatim and the user can decide what to compare).
    pub date_obs: Option<String>,
}

/// Format-aware dispatcher: pulls the canonical FITS keys (UPPERCASE) out
/// of the stored blob into a HashMap.
pub fn parse_stored_header_keys(format: FileFormat, header_text: &str) -> HashMap<String, String> {
    match format {
        FileFormat::FITS => parse_fits_card_text(header_text),
        FileFormat::XISF => parse_xisf_xml_text(header_text),
    }
}

/// Map a parsed-keyword HashMap onto the canonical snapshot fields.
pub fn snapshot_from_keys(frame_id: i64, keys: &HashMap<String, String>) -> FrameOriginalSnapshot {
    let get = |k: &str| keys.get(k).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());

    let object = get("OBJECT");
    let filter = get("FILTER");
    let raw_imagetyp = get("IMAGETYP");
    let imagetyp = raw_imagetyp.as_ref().map(|s| {
        ImageType::from_str(s)
            .map(|t| format!("{:?}", t))
            .unwrap_or_else(|| s.clone())
    });
    let instrume = get("INSTRUME");
    let telescop = get("TELESCOP");
    let focallen = get("FOCALLEN").and_then(|s| s.parse::<f64>().ok());
    // Many cameras store gain under EGAIN and software-set gain under GAIN —
    // prefer the simpler GAIN keyword first, falling back to EGAIN.
    let gain = get("GAIN")
        .and_then(|s| s.parse::<f64>().ok())
        .or_else(|| get("EGAIN").and_then(|s| s.parse::<f64>().ok()));
    let offset = get("OFFSET").and_then(|s| s.parse::<f64>().ok());

    // Binning: prefer XBINNING/YBINNING (numeric pair), fall back to a raw
    // BINNING text keyword if present.
    let xb = get("XBINNING").and_then(|s| s.parse::<i64>().ok());
    let yb = get("YBINNING").and_then(|s| s.parse::<i64>().ok());
    let binning = match (xb, yb) {
        (Some(x), Some(y)) => Some(format!("{}x{}", x, y)),
        _ => get("BINNING"),
    };

    let exptime = get("EXPTIME").and_then(|s| s.parse::<f64>().ok());
    let ccd_temp = get("CCD-TEMP")
        .and_then(|s| s.parse::<f64>().ok())
        .or_else(|| get("CCDTEMP").and_then(|s| s.parse::<f64>().ok()));
    let date_obs = get("DATE-OBS").or_else(|| get("DATE_OBS"));

    FrameOriginalSnapshot {
        frame_id,
        object,
        filter,
        imagetyp,
        instrume,
        telescop,
        focallen,
        gain,
        offset,
        binning,
        exptime,
        ccd_temp,
        date_obs,
    }
}

/// Parse FITS header text where each line is one 80-char card (the format
/// `to_header_text` produces). Skips END/COMMENT/HISTORY/blank cards. Handles
/// quoted-string and unquoted-numeric value forms; comments after `/` are
/// stripped from numeric/unquoted values.
fn parse_fits_card_text(text: &str) -> HashMap<String, String> {
    let mut result: HashMap<String, String> = HashMap::new();
    for raw_line in text.lines() {
        // Cards may have been stripped of trailing spaces during persistence.
        // Treat the line as the card image, padding to 80 if shorter.
        let line = raw_line.trim_end_matches('\r');
        let keyword = line.get(0..line.len().min(8)).unwrap_or("").trim().to_uppercase();
        if keyword.is_empty() || keyword == "END" || keyword == "COMMENT" || keyword == "HISTORY" {
            continue;
        }
        let indicator = line.get(8..10).unwrap_or("");
        if indicator != "= " {
            // HIERARCH and other long-keyword cards aren't typed by users;
            // skip rather than misparse.
            continue;
        }
        let value_area = line.get(10..).unwrap_or("").trim_start();
        let value = if let Some(stripped) = value_area.strip_prefix('\'') {
            // Quoted string: take everything up to the next unescaped quote.
            // FITS escapes quotes by doubling (''), but the simple path is
            // good enough for user-facing fields.
            match stripped.find('\'') {
                Some(end) => stripped[..end].trim_end().to_string(),
                None => continue,
            }
        } else {
            // Numeric/boolean — split off comment after '/' and trim.
            value_area
                .split('/')
                .next()
                .unwrap_or("")
                .trim()
                .to_string()
        };
        if value.is_empty() {
            continue;
        }
        result.entry(keyword).or_insert(value);
    }
    result
}

/// Parse XISF XML text — `FITSKeyword name="..." value="..."` elements,
/// with quotes stripped from values to match how `parse_xisf` already
/// records them.
fn parse_xisf_xml_text(text: &str) -> HashMap<String, String> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut result: HashMap<String, String> = HashMap::new();
    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(ref e)) | Ok(Event::Start(ref e))
                if e.name().as_ref() == b"FITSKeyword" =>
            {
                let mut name = String::new();
                let mut value = String::new();
                for attr in e.attributes().flatten() {
                    match attr.key.as_ref() {
                        b"name" => name = String::from_utf8_lossy(&attr.value).to_string(),
                        b"value" => {
                            let mut v = String::from_utf8_lossy(&attr.value).to_string();
                            if v.starts_with('\'') && v.ends_with('\'') && v.len() >= 2 {
                                v = v[1..v.len() - 1].trim().to_string();
                            }
                            value = v;
                        }
                        _ => {}
                    }
                }
                if !name.is_empty() {
                    result.entry(name.to_uppercase()).or_insert(value);
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fits_card_text_basic_keys() {
        // Realistic header card text with 80-char-padded cards joined by \n.
        let card_object = format!(
            "{:<80}",
            "OBJECT  = 'M42                 '           / Target name"
        );
        let card_exptime = format!(
            "{:<80}",
            "EXPTIME =                  120.0 / Exposure time in seconds"
        );
        let card_ccdtemp = format!(
            "{:<80}",
            "CCD-TEMP=                  -10.0 / CCD temperature C"
        );
        let card_xbin = format!("{:<80}", "XBINNING=                    1 / X binning");
        let card_ybin = format!("{:<80}", "YBINNING=                    1 / Y binning");
        let text = format!(
            "{}\n{}\n{}\n{}\n{}",
            card_object, card_exptime, card_ccdtemp, card_xbin, card_ybin
        );

        let keys = parse_fits_card_text(&text);
        assert_eq!(keys.get("OBJECT"), Some(&"M42".to_string()));
        assert_eq!(keys.get("EXPTIME"), Some(&"120.0".to_string()));
        assert_eq!(keys.get("CCD-TEMP"), Some(&"-10.0".to_string()));
        assert_eq!(keys.get("XBINNING"), Some(&"1".to_string()));
        assert_eq!(keys.get("YBINNING"), Some(&"1".to_string()));
    }

    #[test]
    fn snapshot_normalises_imagetyp_and_binning() {
        let mut keys = HashMap::new();
        keys.insert("IMAGETYP".into(), "MASTER DARK".into());
        keys.insert("XBINNING".into(), "2".into());
        keys.insert("YBINNING".into(), "2".into());
        keys.insert("EXPTIME".into(), "60".into());
        keys.insert("CCD-TEMP".into(), "-9.9".into());

        let snap = snapshot_from_keys(42, &keys);
        assert_eq!(snap.imagetyp.as_deref(), Some("MasterDark"));
        assert_eq!(snap.binning.as_deref(), Some("2x2"));
        assert_eq!(snap.exptime, Some(60.0));
        assert_eq!(snap.ccd_temp, Some(-9.9));
    }

    #[test]
    fn parses_xisf_xml_fitskeyword_elements() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<xisf>
  <FITSKeyword name="OBJECT" value="'M42'" comment="" />
  <FITSKeyword name="EXPTIME" value="120.0" />
  <FITSKeyword name="CCD-TEMP" value="-10.0" />
  <FITSKeyword name="XBINNING" value="1" />
  <FITSKeyword name="YBINNING" value="1" />
</xisf>"#;
        let keys = parse_xisf_xml_text(xml);
        assert_eq!(keys.get("OBJECT"), Some(&"M42".to_string()));
        assert_eq!(keys.get("EXPTIME"), Some(&"120.0".to_string()));
        assert_eq!(keys.get("CCD-TEMP"), Some(&"-10.0".to_string()));
        assert_eq!(keys.get("XBINNING"), Some(&"1".to_string()));
    }
}

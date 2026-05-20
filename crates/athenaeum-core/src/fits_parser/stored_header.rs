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
    /// Pixel size in µm (FITS XPIXSZ). Required alongside FOCALLEN for the
    /// plate-solve scale hint; surfaced here so the metadata pane can revert
    /// to the header value just like any other numeric field.
    pub xpixsz: Option<f64>,
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
    /// Right Ascension in decimal degrees, derived from OBJCTRA/RA in the
    /// header. Sexagesimal OBJCTRA is parsed via `parse_ra_sexagesimal`;
    /// fallback to numeric RA. Plate solving overrides this; the metadata
    /// pane shows the FITS-header original next to the catalog value so the
    /// user can revert a plate solve back to whatever the file shipped with.
    pub ra: Option<f64>,
    /// Declination in decimal degrees. Same precedence as `ra`: OBJCTDEC
    /// (sexagesimal) preferred, numeric DEC as fallback.
    pub dec: Option<f64>,
    /// Image position angle in degrees (north through east). Derived from
    /// the same 8-keyword priority chain as the scanner — CROTA2 → CD-matrix
    /// → CROTA1 → POSANGLE → PA → OBJCTROT → ROTATANG → APTS_ROT — so the
    /// snapshot value matches what the scanner stored in `frames.rotation`,
    /// avoiding a phantom diff in the metadata pane's revert UI. Sky-frame
    /// keywords come first; mechanical rotator angles (ROTATANG, APTS_ROT)
    /// last because they reflect the rotator's hardware position rather
    /// than the image's sky orientation. Plate solving overrides the
    /// catalog value from the CD matrix.
    pub rotation: Option<f64>,
}

/// Format-aware dispatcher: pulls the canonical FITS keys (UPPERCASE) out
/// of the stored blob into a HashMap.
pub fn parse_stored_header_keys(format: FileFormat, header_text: &str) -> HashMap<String, String> {
    let primary = match format {
        FileFormat::FITS => parse_fits_card_text(header_text),
        FileFormat::XISF => parse_xisf_xml_text(header_text),
    };
    if !primary.is_empty() {
        return primary;
    }
    // Some capture tools (notably ASIAIR) persist the header as a plain
    // "KEY = value" text dump rather than 80-col FITS cards or XISF XML, so
    // the format-specific parser yields nothing. Fall back to a line parser
    // so "revert to original" / metadata snapshots still work.
    parse_keyword_eq_text(header_text)
}

/// Parse a plain "KEY = value" header dump (one keyword per line, banner /
/// separator / COMMENT / HISTORY / blank lines skipped). Quoted strings have
/// their quotes stripped; an unquoted value's trailing " / comment" is
/// dropped. Keys are upper-cased to match the other parsers.
fn parse_keyword_eq_text(text: &str) -> HashMap<String, String> {
    let mut result: HashMap<String, String> = HashMap::new();
    for raw in text.lines() {
        let line = raw.trim();
        let Some((k, v)) = line.split_once('=') else { continue };
        let key = k.trim().to_uppercase();
        if key.is_empty()
            || key.contains(' ')
            || key == "END"
            || key == "COMMENT"
            || key == "HISTORY"
        {
            continue;
        }
        let v = v.trim();
        let value = if let Some(s) = v.strip_prefix('\'') {
            match s.find('\'') {
                Some(end) => s[..end].trim_end().to_string(),
                None => continue,
            }
        } else {
            // Drop a FITS-style trailing comment (" / ...") without breaking
            // space-separated values like "2 41 04.824".
            v.split(" / ").next().unwrap_or("").trim().to_string()
        };
        if value.is_empty() {
            continue;
        }
        result.entry(key).or_insert(value);
    }
    result
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
    let xpixsz = get("XPIXSZ").and_then(|s| s.parse::<f64>().ok());
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

    // RA: prefer sexagesimal OBJCTRA (HH:MM:SS); fall back to numeric RA in
    // decimal degrees. Numeric RA values from a FITS header are typically
    // already in degrees, but we accept both ":"-separated and bare-decimal
    // forms — `parse_ra_sexagesimal` only handles the former, so we route
    // accordingly.
    let ra = get("OBJCTRA")
        .and_then(|s| crate::coordinates::parse_ra_sexagesimal(&s).ok())
        .or_else(|| get("RA").and_then(|s| s.parse::<f64>().ok()));
    let dec = get("OBJCTDEC")
        .and_then(|s| crate::coordinates::parse_dec_sexagesimal(&s).ok())
        .or_else(|| get("DEC").and_then(|s| s.parse::<f64>().ok()));
    // Rotation: must match the scanner's keyword priority in
    // `fits_parser/mod.rs::extract_metadata` exactly, otherwise the
    // metadata pane shows a phantom "original differs from current" diff
    // for any frame whose header has multiple rotation-bearing keywords.
    //
    // Sky-frame keywords first; mechanical-rotator-angle keywords last —
    // ROTATANG / APTS_ROT are raw hardware positions whose mapping to the
    // sky depends on rotator zero-offset. Real-world example: NINA writes
    // both OBJCTROT=185 (planned sky rotation) and ROTATANG=208 (mechanical
    // angle); the rotator was offset 23° from sky-east-of-north.
    //
    //   1. CROTA2 — FITS WCS standard (sky frame)
    //   2. CD matrix — atan2(-CD1_2, CD2_2) (sky frame)
    //   3. CROTA1 — alt WCS axis rotation (sky frame)
    //   4. POSANGLE — sky position angle
    //   5. PA — generic position angle (sky frame)
    //   6. OBJCTROT — planned target rotation (NINA / PI, sky frame)
    //   7. ROTATANG — mechanical rotator angle (instrument frame, fallback)
    //   8. APTS_ROT — APT mechanical rotator angle (instrument frame)
    let parse_f = |k: &str| get(k).and_then(|s| s.parse::<f64>().ok());
    let rotation = parse_f("CROTA2")
        .or_else(|| match (parse_f("CD1_2"), parse_f("CD2_2")) {
            (Some(cd1_2), Some(cd2_2)) => Some((-cd1_2).atan2(cd2_2).to_degrees()),
            _ => None,
        })
        .or_else(|| parse_f("CROTA1"))
        .or_else(|| parse_f("POSANGLE"))
        .or_else(|| parse_f("PA"))
        .or_else(|| parse_f("OBJCTROT"))
        .or_else(|| parse_f("ROTATANG"))
        .or_else(|| parse_f("APTS_ROT"));

    FrameOriginalSnapshot {
        frame_id,
        object,
        filter,
        imagetyp,
        instrume,
        telescop,
        focallen,
        xpixsz,
        gain,
        offset,
        binning,
        exptime,
        ccd_temp,
        date_obs,
        ra,
        dec,
        rotation,
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
    fn parses_asiair_xisf_keyword_dump() {
        // ASIAIR persists the XISF header as a plain "KEY = value" text dump
        // (not XISF XML, not 80-col FITS cards). parse_xisf_xml_text returns
        // empty for this; the dispatcher must fall back to the line parser.
        let dump = "XISF FITS Keywords:\n==================\n\nSIMPLE = T\nBITPIX = 16\nNAXIS = 2\nCOMMENT = \nFOCALLEN = 140.616\nXPIXSZ = 2.4\nOBJCTRA = 2 41 04.824\nOBJCTDEC = +62 56 44.95\nCROTA2 = 4.383973760472021\nCREATOR = ZWO ASIAIR\n";
        // XML parser must yield nothing for this format.
        assert!(parse_xisf_xml_text(dump).is_empty());
        // Dispatcher must recover the keys via the fallback.
        let keys = parse_stored_header_keys(FileFormat::XISF, dump);
        assert_eq!(keys.get("FOCALLEN"), Some(&"140.616".to_string()));
        assert_eq!(keys.get("OBJCTRA"), Some(&"2 41 04.824".to_string()));
        assert_eq!(keys.get("OBJCTDEC"), Some(&"+62 56 44.95".to_string()));
        // And the snapshot must materialise the originals.
        let snap = snapshot_from_keys(7, &keys);
        assert_eq!(snap.focallen, Some(140.616));
        let ra = snap.ra.expect("ra");
        let dec = snap.dec.expect("dec");
        assert!((ra - 40.2701).abs() < 0.01, "ra={ra}");
        assert!((dec - 62.9458).abs() < 0.01, "dec={dec}");
        assert!(snap.rotation.map(|r| (r - 4.384).abs() < 0.01).unwrap_or(false));
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
    fn objctrot_beats_rotatang() {
        // Real-world case: NINA writes both OBJCTROT (planned sky rotation)
        // and ROTATANG (mechanical hardware angle). OBJCTROT is in sky frame
        // and reflects user intent; ROTATANG is the raw rotator position
        // whose mapping to sky depends on the rotator's zero offset. Sky
        // frame wins.
        let mut keys = HashMap::new();
        keys.insert("OBJCTROT".into(), "185.0".into());
        keys.insert("ROTATANG".into(), "208.089996337891".into());
        let snap = snapshot_from_keys(42, &keys);
        assert_eq!(
            snap.rotation,
            Some(185.0),
            "OBJCTROT (sky frame) must beat ROTATANG (mechanical)"
        );
    }

    #[test]
    fn rotation_full_chain_priority() {
        // Walk the chain top-down: each keyword wins over everything below it.
        // Order: CROTA2 → CD-matrix → CROTA1 → POSANGLE → PA → OBJCTROT →
        // ROTATANG → APTS_ROT. Sky-frame keys precede mechanical ones.
        let mut keys = HashMap::new();
        keys.insert("APTS_ROT".into(), "8.0".into());
        keys.insert("ROTATANG".into(), "7.0".into());
        keys.insert("OBJCTROT".into(), "6.0".into());
        keys.insert("PA".into(), "5.0".into());
        keys.insert("POSANGLE".into(), "4.0".into());
        keys.insert("CROTA1".into(), "3.0".into());
        keys.insert("CROTA2".into(), "1.0".into());
        let snap = snapshot_from_keys(1, &keys);
        assert_eq!(snap.rotation, Some(1.0), "CROTA2 must win when present");

        // Drop CROTA2 → CD matrix wins next (atan2-derived).
        keys.remove("CROTA2");
        keys.insert("CD1_2".into(), "0.0".into());
        keys.insert("CD2_2".into(), "1.0".into());
        let snap = snapshot_from_keys(1, &keys);
        // atan2(-0.0, 1.0) = 0.0 rad = 0°
        assert_eq!(snap.rotation, Some(0.0), "CD matrix must win over CROTA1+");

        // Remove CD matrix → CROTA1 wins.
        keys.remove("CD1_2");
        keys.remove("CD2_2");
        let snap = snapshot_from_keys(1, &keys);
        assert_eq!(snap.rotation, Some(3.0));

        // ...POSANGLE...
        keys.remove("CROTA1");
        let snap = snapshot_from_keys(1, &keys);
        assert_eq!(snap.rotation, Some(4.0));

        // ...PA (now 5th)...
        keys.remove("POSANGLE");
        let snap = snapshot_from_keys(1, &keys);
        assert_eq!(snap.rotation, Some(5.0));

        // ...OBJCTROT (now 6th)...
        keys.remove("PA");
        let snap = snapshot_from_keys(1, &keys);
        assert_eq!(snap.rotation, Some(6.0));

        // ...ROTATANG (now 7th, demoted from 5th — sky frame wins over hw)...
        keys.remove("OBJCTROT");
        let snap = snapshot_from_keys(1, &keys);
        assert_eq!(snap.rotation, Some(7.0));

        // ...APTS_ROT.
        keys.remove("ROTATANG");
        let snap = snapshot_from_keys(1, &keys);
        assert_eq!(snap.rotation, Some(8.0));

        // No rotation keys at all → None.
        keys.clear();
        let snap = snapshot_from_keys(1, &keys);
        assert_eq!(snap.rotation, None);
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

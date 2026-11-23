//! Parse a header blob that's been stored in `fits_header.header` back into
//! a keyword map, then map known keys to the canonical fields the catalog
//! cares about.
//!
//! The scanner stores these formats in the same TEXT column:
//!   - **FITS**: 80-byte cards joined with `\n` (see `FitsHeader::to_header_text`).
//!   - **XISF**: the `NAME = value` keyword dump of `xisf_header_text` (Athenaeum's own
//!     output), or, in older catalogs, the raw `<?xml...</xisf>` block.
//!
//! The reading itself lives in `header_keys`: one key reader per format, the scanner's own
//! (HIERARCH names, doubled quotes and CONTINUE cards for FITS; the `FITSKeyword` extraction for
//! XISF). Re-parsing here lets the UI show "what the file looked like at the most
//! recent scan" so a user can compare against their custom edits and revert
//! individual fields if they want to.
//!
//! Note: this is intentionally line-based (rather than byte-stream) so the
//! stored text — which has cards joined by newlines, not contiguous 2880-byte
//! blocks — round-trips cleanly without padding/realigning.

use super::header_keys::{stored_header_key_map, HeaderKeyMap};
use crate::header_fallbacks::{resolve_all, HeaderFallbackConfig};
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

/// Format-aware dispatcher: pulls the canonical FITS keys (UPPERCASE, `COMMENT` / `HISTORY`
/// dropped, values trimmed, blank ones dropped) out of the stored blob into a HashMap. A thin
/// view of [`stored_header_key_map`], which holds the readers.
pub fn parse_stored_header_keys(format: FileFormat, header_text: &str) -> HashMap<String, String> {
    stored_header_key_map(format, header_text).into_legacy_map()
}

/// Map a stored header's key map onto the canonical snapshot fields. The ten fields the rules
/// govern come from the one resolver under `rules`, so the pane's "original" column agrees with
/// what a scan under the same rules stored.
pub fn snapshot_from_keys(
    frame_id: i64,
    keys: &HeaderKeyMap,
    rules: &HeaderFallbackConfig,
) -> FrameOriginalSnapshot {
    let get = |k: &str| keys.get(k).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let r = resolve_all(rules, keys);

    let raw_imagetyp = get("IMAGETYP");
    let imagetyp = raw_imagetyp.as_ref().map(|s| {
        ImageType::from_str(s)
            .map(|t| format!("{:?}", t))
            .unwrap_or_else(|| s.clone())
    });

    // Binning: prefer XBINNING/YBINNING (numeric pair), fall back to a raw
    // BINNING text keyword if present.
    let xb = get("XBINNING").and_then(|s| s.parse::<i64>().ok());
    let yb = get("YBINNING").and_then(|s| s.parse::<i64>().ok());
    let binning = match (xb, yb) {
        (Some(x), Some(y)) => Some(format!("{}x{}", x, y)),
        _ => get("BINNING"),
    };

    let date_obs = get("DATE-OBS").or_else(|| get("DATE_OBS"));

    // RA: prefer sexagesimal OBJCTRA (HH:MM:SS); fall back to numeric RA in
    // decimal degrees, then to CRVAL1 (FITS WCS reference-pixel sky RA in
    // degrees — present on plate-solved/survey frames whose embedded WCS
    // describes the same astrometry the metadata pane wants to revert to).
    // Without the CRVAL1 fallback, ra/dec would clear on revert for any
    // file whose header has CRVAL1/CRVAL2 but no OBJCTRA/RA — out of step
    // with `rotation`, which already picks up CD-matrix derived values.
    let ra = get("OBJCTRA")
        .and_then(|s| crate::coordinates::parse_ra_sexagesimal(&s).ok())
        .or_else(|| get("RA").and_then(|s| s.parse::<f64>().ok()))
        .or_else(|| get("CRVAL1").and_then(|s| s.parse::<f64>().ok()));
    let dec = get("OBJCTDEC")
        .and_then(|s| crate::coordinates::parse_dec_sexagesimal(&s).ok())
        .or_else(|| get("DEC").and_then(|s| s.parse::<f64>().ok()))
        .or_else(|| get("CRVAL2").and_then(|s| s.parse::<f64>().ok()));
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
    //   6. OBJCTROT — planned target rotation (NINA / the ERP, sky frame)
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
        object: r.object,
        filter: r.filter,
        imagetyp,
        instrume: r.instrume,
        telescop: r.telescop,
        focallen: r.focallen,
        xpixsz: r.xpixsz,
        gain: r.gain,
        offset: r.offset,
        binning,
        exptime: r.exptime,
        ccd_temp: r.ccd_temp,
        date_obs,
        ra,
        dec,
        rotation,
    }
}

/// The four CFA identity fields as stored in a raw header, parsed with the
/// same leniency as [`snapshot_from_keys`]. Consumers back-fill catalog
/// columns from these when a frame snapshot arrived without them: sync
/// ingest from a sender whose `frame_meta` was built by a CFA-blind
/// projection, and the one-time catalog repair for rows already landed
/// that way (`db::repair`).
pub struct CfaHeaderFields {
    pub bayerpat: Option<String>,
    pub xbayroff: Option<i64>,
    pub ybayroff: Option<i64>,
    pub roworder: Option<String>,
}

pub fn cfa_fields_from_keys(keys: &HashMap<String, String>) -> CfaHeaderFields {
    let get = |k: &str| keys.get(k).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    // Kept in lock-step with the scanner's `parse_offset` in
    // `build_frame_from_header`: some writers spell an integer-valued offset as
    // a float ("1.0"). A strict integer parse would silently drop it to
    // "unknown" here while the scanner lands it.
    let offset = |k: &str| -> Option<i64> {
        get(k).and_then(|s| {
            s.parse::<i64>().ok().or_else(|| s.parse::<f64>().ok().map(|f| f as i64))
        })
    };
    CfaHeaderFields {
        bayerpat: get("BAYERPAT"),
        xbayroff: offset("XBAYROFF"),
        ybayroff: offset("YBAYROFF"),
        roworder: get("ROWORDER"),
    }
}

/// Fill a [`Frame`](crate::models::Frame)'s missing CFA fields from the file's
/// own raw header. Only `None` fields are filled — a value the snapshot carried
/// always wins.
pub fn backfill_frame_cfa(
    frame: &mut crate::models::Frame,
    format: FileFormat,
    header_text: &str,
) {
    if frame.bayerpat.is_some()
        && frame.xbayroff.is_some()
        && frame.ybayroff.is_some()
        && frame.roworder.is_some()
    {
        return;
    }
    let keys = parse_stored_header_keys(format, header_text);
    let cfa = cfa_fields_from_keys(&keys);
    if frame.bayerpat.is_none() {
        frame.bayerpat = cfa.bayerpat;
    }
    if frame.xbayroff.is_none() {
        frame.xbayroff = cfa.xbayroff;
    }
    if frame.ybayroff.is_none() {
        frame.ybayroff = cfa.ybayroff;
    }
    if frame.roworder.is_none() {
        frame.roworder = cfa.roworder;
    }
}

/// `APTDIA` (mm) of a stored header blob — finite and positive, else `None`
/// (plan 6b-2 C3/C4). `format` is `files.format` (`"FITS"` / `"XISF"`); any
/// other value reads as no aperture.
pub fn aperture_mm_from_stored_header(format: &str, header: &str) -> Option<f64> {
    let format = match format {
        "FITS" => FileFormat::FITS,
        "XISF" => FileFormat::XISF,
        _ => return None,
    };
    parse_stored_header_keys(format, header)
        .get("APTDIA")
        .and_then(|v| v.trim().trim_matches('\'').trim().parse::<f64>().ok())
        .filter(|a| a.is_finite() && *a > 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> HeaderFallbackConfig {
        HeaderFallbackConfig::default()
    }

    /// Key map of `(key, value)` pairs (first wins), the shape the readers produce.
    fn km(pairs: &[(&str, &str)]) -> HeaderKeyMap {
        HeaderKeyMap::from_first_wins(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())))
    }

    fn snap(frame_id: i64, pairs: &[(&str, &str)]) -> FrameOriginalSnapshot {
        snapshot_from_keys(frame_id, &km(pairs), &rules())
    }

    #[test]
    fn aperture_reads_aptdia_finite_and_positive_only() {
        let fits = |v: &str| {
            format!(
                "{:<80}{:<80}",
                format!("APTDIA  = {v:>20} / aperture"),
                "END"
            )
        };
        assert_eq!(
            aperture_mm_from_stored_header("FITS", &fits("200.0")),
            Some(200.0)
        );
        assert_eq!(aperture_mm_from_stored_header("FITS", &fits("0")), None);
        assert_eq!(aperture_mm_from_stored_header("FITS", &fits("-5")), None);
        assert_eq!(
            aperture_mm_from_stored_header("FITS", &format!("{:<80}", "END")),
            None
        );
        assert_eq!(aperture_mm_from_stored_header("ZIP", &fits("200.0")), None);
    }

    /// The scanner's one-rule skip for Athenaeum artifacts keys on
    /// `ATH_STK` being visible in the stored header (spec §6.4). An XISF
    /// master (M4d Task 2) must therefore surface that keyword through this
    /// very path — `extract_xisf_header` + `parse_stored_header_keys` —
    /// which is what makes "a master is never cataloged" true for the XISF
    /// container too instead of resting on three unrelated details.
    #[test]
    fn an_xisf_masters_stored_header_surfaces_ath_stk() {
        use crate::fits_writer::{write_xisf_f32, Card, CardValue};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("master.xisf");
        let cards = vec![
            Card::new("IMAGETYP", CardValue::Str("Master Light".into())).unwrap(),
            Card::new("ATH_STK", CardValue::Logical(true))
                .unwrap()
                .with_comment("stacked by Athenaeum; never cataloged"),
            Card::new("OBJECT", CardValue::Str("LDN 1272".into())).unwrap(),
        ];
        write_xisf_f32(&path, 4, 2, 1, &[0.25f32; 8], &cards).unwrap();

        let header = crate::fits_parser::extract_xisf_header(&path).unwrap();
        let keys = parse_stored_header_keys(FileFormat::XISF, &header);
        assert_eq!(keys.get("ATH_STK").map(String::as_str), Some("T"), "{keys:?}");
        assert_eq!(
            keys.get("IMAGETYP").map(String::as_str),
            Some("Master Light"),
            "{keys:?}"
        );
        assert_eq!(keys.get("OBJECT").map(String::as_str), Some("LDN 1272"));
    }

    /// Survey / processed frames (e.g. SkyMapper `1_18_r_1_P.fit`) ship
    /// with full FITS WCS — `CRVAL1`/`CRVAL2` + a CD matrix — but no
    /// OBJCTRA/RA or OBJCTDEC/DEC. The snapshot must extract ra/dec from
    /// CRVAL* so the metadata pane's revert can restore them; otherwise
    /// the FITS-header column shows blank ra/dec while rotation is
    /// populated (from CD matrix) and "Revert WCS to header" clears
    /// ra/dec but leaves rotation, surprising the user.
    #[test]
    fn snapshot_falls_back_to_crval_when_objctra_missing() {
        // Header with WCS only — no OBJCTRA / RA / OBJCTDEC / DEC.
        let snap = snap(42, &[
            ("CRVAL1", "234.6002"),
            ("CRVAL2", "-33.2365"),
            ("CD1_2", "-1.2e-7"),
            ("CD2_2", "0.000138"),
        ]);
        let ra = snap.ra.expect("ra must come from CRVAL1");
        let dec = snap.dec.expect("dec must come from CRVAL2");
        assert!((ra - 234.6002).abs() < 1e-6, "ra={ra}");
        assert!((dec - -33.2365).abs() < 1e-6, "dec={dec}");
        // CD-matrix-derived rotation still works (this is what motivated
        // the bug — extracting it but NOT ra/dec was the inconsistency).
        assert!(snap.rotation.is_some(), "rotation must be derived from CD matrix");

        // Sanity: OBJCTRA wins over CRVAL1 when present, so survey frames
        // that DO have OBJCTRA continue to use it.
        let snap2 = self::snap(43, &[("OBJCTRA", "15 00 00"), ("CRVAL1", "999.0")]);
        let ra2 = snap2.ra.expect("ra");
        assert!((ra2 - 225.0).abs() < 0.01, "OBJCTRA must win over CRVAL1; got {ra2}");
    }

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

        let keys = parse_stored_header_keys(FileFormat::FITS, &text);
        assert_eq!(keys.get("OBJECT"), Some(&"M42".to_string()));
        assert_eq!(keys.get("EXPTIME"), Some(&"120.0".to_string()));
        assert_eq!(keys.get("CCD-TEMP"), Some(&"-10.0".to_string()));
        assert_eq!(keys.get("XBINNING"), Some(&"1".to_string()));
        assert_eq!(keys.get("YBINNING"), Some(&"1".to_string()));
    }

    #[test]
    fn parses_the_xisf_keyword_dump() {
        // Athenaeum stores an XISF header as the `NAME = value` dump of its own
        // `xisf_header_text` (banner, then one line per FITSKeyword). The dispatcher
        // inverts that dump.
        let dump = "XISF FITS Keywords:\n==================\n\nSIMPLE = T\nBITPIX = 16\nNAXIS = 2\nCOMMENT = \nFOCALLEN = 140.616\nXPIXSZ = 2.4\nOBJCTRA = 2 41 04.824\nOBJCTDEC = +62 56 44.95\nCROTA2 = 4.383973760472021\nCREATOR = ZWO ASIAIR\n";
        let keys = parse_stored_header_keys(FileFormat::XISF, dump);
        assert_eq!(keys.get("FOCALLEN"), Some(&"140.616".to_string()));
        assert_eq!(keys.get("OBJCTRA"), Some(&"2 41 04.824".to_string()));
        assert_eq!(keys.get("OBJCTDEC"), Some(&"+62 56 44.95".to_string()));
        // And the snapshot must materialise the originals.
        let snap = snapshot_from_keys(7, &stored_header_key_map(FileFormat::XISF, dump), &rules());
        assert_eq!(snap.focallen, Some(140.616));
        let ra = snap.ra.expect("ra");
        let dec = snap.dec.expect("dec");
        assert!((ra - 40.2701).abs() < 0.01, "ra={ra}");
        assert!((dec - 62.9458).abs() < 0.01, "dec={dec}");
        assert!(snap.rotation.map(|r| (r - 4.384).abs() < 0.01).unwrap_or(false));
    }

    #[test]
    fn snapshot_normalises_imagetyp_and_binning() {
        let snap = snap(42, &[
            ("IMAGETYP", "MASTER DARK"),
            ("XBINNING", "2"),
            ("YBINNING", "2"),
            ("EXPTIME", "60"),
            ("CCD-TEMP", "-9.9"),
        ]);
        assert_eq!(snap.imagetyp.as_deref(), Some("MasterDark"));
        assert_eq!(snap.binning.as_deref(), Some("2x2"));
        assert_eq!(snap.exptime, Some(60.0));
        assert_eq!(snap.ccd_temp, Some(-9.9));
    }

    /// The snapshot (metadata pane's "original" column) must agree with what
    /// the scanner stored in `frames.ccd_temp`, or the pane shows a phantom
    /// diff: `DET-TEMP` fills in when `CCD-TEMP` is absent or unparseable and
    /// never outranks it.
    #[test]
    fn snapshot_ccd_temp_falls_back_to_det_temp() {
        let snap_of = |pairs: &[(&str, &str)]| snap(1, pairs).ccd_temp;
        assert_eq!(snap_of(&[("DET-TEMP", "-5.5")]), Some(-5.5), "DET-TEMP alone");
        assert_eq!(
            snap_of(&[("CCD-TEMP", "-10.0"), ("DET-TEMP", "-5.5")]),
            Some(-10.0),
            "CCD-TEMP wins when both are present"
        );
        assert_eq!(
            snap_of(&[("CCD-TEMP", "n/a"), ("DET-TEMP", "-5.5")]),
            Some(-5.5),
            "unparseable CCD-TEMP defers to DET-TEMP"
        );
        assert_eq!(snap_of(&[("EXPTIME", "60")]), None, "neither keyword → unset");
    }

    /// Same fallback through the real stored-header forms: 80-column FITS card
    /// text and XISF XML.
    #[test]
    fn det_temp_reaches_the_snapshot_from_stored_header_text() {
        let fits = format!("{:<80}\n{:<80}", "SIMPLE  =                    T", "DET-TEMP=                 -5.5 / sensor");
        let keys = stored_header_key_map(FileFormat::FITS, &fits);
        assert_eq!(snapshot_from_keys(1, &keys, &rules()).ccd_temp, Some(-5.5));

        let xisf = r#"<?xml version="1.0" encoding="UTF-8"?>
<xisf>
  <FITSKeyword name="DET-TEMP" value="-5.5" />
</xisf>"#;
        let keys = stored_header_key_map(FileFormat::XISF, xisf);
        assert_eq!(snapshot_from_keys(1, &keys, &rules()).ccd_temp, Some(-5.5));
    }

    #[test]
    fn objctrot_beats_rotatang() {
        // Real-world case: NINA writes both OBJCTROT (planned sky rotation)
        // and ROTATANG (mechanical hardware angle). OBJCTROT is in sky frame
        // and reflects user intent; ROTATANG is the raw rotator position
        // whose mapping to sky depends on the rotator's zero offset. Sky
        // frame wins.
        let snap = snap(42, &[("OBJCTROT", "185.0"), ("ROTATANG", "208.089996337891")]);
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
        let mut keys: Vec<(&str, &str)> = Vec::new();
        keys.push(("APTS_ROT", "8.0"));
        keys.push(("ROTATANG", "7.0"));
        keys.push(("OBJCTROT", "6.0"));
        keys.push(("PA", "5.0"));
        keys.push(("POSANGLE", "4.0"));
        keys.push(("CROTA1", "3.0"));
        keys.push(("CROTA2", "1.0"));
        let snap = self::snap(1, &keys);
        assert_eq!(snap.rotation, Some(1.0), "CROTA2 must win when present");

        // Drop CROTA2 → CD matrix wins next (atan2-derived).
        keys.retain(|(k, _)| *k != "CROTA2");
        keys.push(("CD1_2", "0.0"));
        keys.push(("CD2_2", "1.0"));
        let snap = self::snap(1, &keys);
        // atan2(-0.0, 1.0) = 0.0 rad = 0°
        assert_eq!(snap.rotation, Some(0.0), "CD matrix must win over CROTA1+");

        // Remove CD matrix → CROTA1 wins.
        keys.retain(|(k, _)| *k != "CD1_2");
        keys.retain(|(k, _)| *k != "CD2_2");
        let snap = self::snap(1, &keys);
        assert_eq!(snap.rotation, Some(3.0));

        // ...POSANGLE...
        keys.retain(|(k, _)| *k != "CROTA1");
        let snap = self::snap(1, &keys);
        assert_eq!(snap.rotation, Some(4.0));

        // ...PA (now 5th)...
        keys.retain(|(k, _)| *k != "POSANGLE");
        let snap = self::snap(1, &keys);
        assert_eq!(snap.rotation, Some(5.0));

        // ...OBJCTROT (now 6th)...
        keys.retain(|(k, _)| *k != "PA");
        let snap = self::snap(1, &keys);
        assert_eq!(snap.rotation, Some(6.0));

        // ...ROTATANG (now 7th, demoted from 5th — sky frame wins over hw)...
        keys.retain(|(k, _)| *k != "OBJCTROT");
        let snap = self::snap(1, &keys);
        assert_eq!(snap.rotation, Some(7.0));

        // ...APTS_ROT.
        keys.retain(|(k, _)| *k != "ROTATANG");
        let snap = self::snap(1, &keys);
        assert_eq!(snap.rotation, Some(8.0));

        // No rotation keys at all → None.
        keys.clear();
        let snap = self::snap(1, &keys);
        assert_eq!(snap.rotation, None);
    }

    #[test]
    fn parses_raw_xisf_xml_fitskeyword_elements() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<xisf>
  <FITSKeyword name="OBJECT" value="'M42'" comment="" />
  <FITSKeyword name="EXPTIME" value="120.0" />
  <FITSKeyword name="CCD-TEMP" value="-10.0" />
  <FITSKeyword name="XBINNING" value="1" />
  <FITSKeyword name="YBINNING" value="1" />
</xisf>"#;
        let keys = parse_stored_header_keys(FileFormat::XISF, xml);
        assert_eq!(keys.get("OBJECT"), Some(&"M42".to_string()));
        assert_eq!(keys.get("EXPTIME"), Some(&"120.0".to_string()));
        assert_eq!(keys.get("CCD-TEMP"), Some(&"-10.0".to_string()));
        assert_eq!(keys.get("XBINNING"), Some(&"1".to_string()));
    }

    /// quick-xml 0.41 rejects a lone `&` (one not opening a character or
    /// entity reference) in element text by default, where 0.36/0.37 read
    /// straight through. We opt back into the permissive behavior, so a stored
    /// header carrying one must still yield its keywords rather than silently
    /// losing the whole snapshot.
    #[test]
    fn dangling_ampersand_in_xisf_text_still_yields_keywords() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<xisf>
  <Property id="note">Dark & Flat calibration</Property>
  <FITSKeyword name="OBJECT" value="'M42'" comment="" />
  <FITSKeyword name="EXPTIME" value="120.0" />
</xisf>"#;
        let keys = parse_stored_header_keys(FileFormat::XISF, xml);

        // Not just "no error": the FITSKeyword elements *after* the dangling
        // `&` must still have been reached and read correctly.
        assert_eq!(
            keys.get("OBJECT"),
            Some(&"M42".to_string()),
            "keywords after a dangling `&` must still parse"
        );
        assert_eq!(keys.get("EXPTIME"), Some(&"120.0".to_string()));
    }

    /// `CCDTEMP` and `EGAIN` were never keywords the scanner read; the snapshot used to
    /// invent them (a phantom diff against `frames.ccd_temp` / `frames.gain`).
    #[test]
    fn snapshot_ignores_ccdtemp_and_egain() {
        let s = snap(1, &[("CCDTEMP", "-10"), ("EGAIN", "0.24")]);
        assert_eq!(s.ccd_temp, None);
        assert_eq!(s.gain, None);
    }

    #[test]
    fn snapshot_reads_exposure_and_pixsize1() {
        let s = snap(1, &[("EXPOSURE", "60"), ("PIXSIZE1", "3.76")]);
        assert_eq!(s.exptime, Some(60.0));
        assert_eq!(s.xpixsz, Some(3.76));
    }

    #[test]
    fn snapshot_follows_a_custom_order() {
        let mut custom = HeaderFallbackConfig::default();
        custom.fields.get_mut(&crate::header_fallbacks::HeaderField::CcdTemp).unwrap().order =
            vec!["DET-TEMP".to_string(), "CCD-TEMP".to_string()];
        let s = snapshot_from_keys(1, &km(&[("CCD-TEMP", "-10"), ("DET-TEMP", "-5.5")]), &custom);
        assert_eq!(s.ccd_temp, Some(-5.5));
    }

    fn fits_text(cards: &[&str]) -> String {
        cards.iter().map(|c| format!("{c:<80}")).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn legacy_map_reads_hierarch_cards() {
        let text = fits_text(&["HIERARCH ESO DET TEMP = -7.25 / sensor", "END"]);
        let keys = parse_stored_header_keys(FileFormat::FITS, &text);
        assert_eq!(keys.get("ESO DET TEMP").map(String::as_str), Some("-7.25"));
    }

    #[test]
    fn legacy_map_joins_continue_cards() {
        let text = fits_text(&[
            "FILTER  = 'Long filter &'",
            "CONTINUE  '  two'",
            "END",
        ]);
        let keys = parse_stored_header_keys(FileFormat::FITS, &text);
        // FitsHeader's own join: the first part is right-trimmed, the continuation verbatim.
        assert_eq!(keys.get("FILTER").map(String::as_str), Some("Long filter  two"));
    }

    #[test]
    fn legacy_map_keeps_a_doubled_quote() {
        // Was "O": the old reader stopped at the first quote. Now the scanner's reading.
        let text = fits_text(&["OBJECT  = 'O''Brien'", "END"]);
        let keys = parse_stored_header_keys(FileFormat::FITS, &text);
        assert_eq!(keys.get("OBJECT").map(String::as_str), Some("O'Brien"));
    }

    #[test]
    fn legacy_map_keeps_an_xisf_value_with_slash() {
        // Was "Ha": the dump line was cut at " / ". Now the value as the XISF builder read it.
        let dump = "XISF FITS Keywords:\n==================\n\nFILTER = Ha / 7nm\n";
        let keys = parse_stored_header_keys(FileFormat::XISF, dump);
        assert_eq!(keys.get("FILTER").map(String::as_str), Some("Ha / 7nm"));
    }

    #[test]
    fn legacy_map_xisf_last_duplicate_wins() {
        // Was "100": the XISF builder keeps the last duplicate, now the stored text agrees.
        let dump = "XISF FITS Keywords:\n==================\n\nGAIN = 100\nGAIN = 120\n";
        let keys = parse_stored_header_keys(FileFormat::XISF, dump);
        assert_eq!(keys.get("GAIN").map(String::as_str), Some("120"));
    }

    #[test]
    fn legacy_map_drops_comment_history_and_blank_values() {
        let text = fits_text(&["COMMENT hello", "HISTORY made", "OBJECT  = ''", "GAIN    = 100", "END"]);
        let keys = parse_stored_header_keys(FileFormat::FITS, &text);
        assert!(!keys.contains_key("COMMENT"));
        assert!(!keys.contains_key("HISTORY"));
        assert!(!keys.contains_key("OBJECT"));
        assert_eq!(keys.get("GAIN").map(String::as_str), Some("100"));
    }
}

#[cfg(test)]
mod cfa_backfill_tests {
    use super::*;
    use crate::models::{FileFormat, Frame};

    // "KEY = value" dump form — explicitly supported by the ASIAIR fallback
    // parser, so the test doesn't depend on 80-col card layout.
    const OSC_HEADER: &str = "BAYERPAT= 'RGGB'\nXBAYROFF= 1\nYBAYROFF= 0\nROWORDER= 'BOTTOM-UP'\nEND";

    fn blank_frame() -> Frame {
        // Frame derives Default — everything None/false except the required id.
        Frame { file_id: 1, ..Default::default() }
    }

    #[test]
    fn cfa_fields_parse_from_keys() {
        let keys = parse_stored_header_keys(FileFormat::FITS, OSC_HEADER);
        let cfa = cfa_fields_from_keys(&keys);
        assert_eq!(cfa.bayerpat.as_deref(), Some("RGGB"));
        assert_eq!(cfa.xbayroff, Some(1));
        assert_eq!(cfa.ybayroff, Some(0));
        assert_eq!(cfa.roworder.as_deref(), Some("BOTTOM-UP"));
    }

    /// Scanner parity: `build_frame_from_header` accepts an integer-valued
    /// offset spelled as a float ("1.0"), so the back-fill helper must too.
    /// The one-time repair stamps its settings flag after running and sets
    /// `bayerpat` non-NULL, so a row under-filled here is never revisited.
    #[test]
    fn float_spelled_cfa_offsets_parse_from_keys() {
        let keys = parse_stored_header_keys(
            FileFormat::FITS,
            "BAYERPAT= 'RGGB'\nXBAYROFF= 1.0\nYBAYROFF= 0.0\nEND",
        );
        let cfa = cfa_fields_from_keys(&keys);
        assert_eq!(cfa.bayerpat.as_deref(), Some("RGGB"));
        assert_eq!(cfa.xbayroff, Some(1), "float-spelled offset must not drop to unknown");
        assert_eq!(cfa.ybayroff, Some(0), "float-spelled offset must not drop to unknown");
    }

    #[test]
    fn backfill_fills_only_missing_fields() {
        let mut frame = blank_frame();
        frame.bayerpat = Some("GBRG".to_string()); // snapshot value must win
        backfill_frame_cfa(&mut frame, FileFormat::FITS, OSC_HEADER);
        assert_eq!(frame.bayerpat.as_deref(), Some("GBRG"), "existing value never overwritten");
        assert_eq!(frame.xbayroff, Some(1));
        assert_eq!(frame.ybayroff, Some(0));
        assert_eq!(frame.roworder.as_deref(), Some("BOTTOM-UP"));
    }

    #[test]
    fn backfill_is_inert_on_a_mono_header() {
        let mut frame = blank_frame();
        backfill_frame_cfa(&mut frame, FileFormat::FITS, "EXPTIME = 300.0\nEND");
        assert!(frame.bayerpat.is_none());
        assert!(frame.xbayroff.is_none());
        assert!(frame.ybayroff.is_none());
        assert!(frame.roworder.is_none());
    }
}

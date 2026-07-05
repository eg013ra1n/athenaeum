//! Header cards for a calibrated LIGHT output (B5 in-app light calibration,
//! design spec 2026-07-05-light-calibration-design.md §7). Pure function:
//! takes the source light's own header cards + the calibration outcome,
//! copies through the metadata that's still valid after calibration, and
//! appends the CALSTAT/ATH_C* provenance cards. No FITS I/O here — Task 3's
//! engine calls this to build the card list it hands to `write_fits_f32`.

use crate::fits_writer::keywords::{FrameKind, HeaderBuilder};
use crate::fits_writer::{Card, CardValue, FitsWriteError};
use anyhow::Result;

/// Header keywords copied verbatim from the source LIGHT frame's own
/// on-disk cards. Categorized per spec §7 ("WCS cards, optics, DATE-OBS,
/// session cards, BAYERPAT/XBAYROFF/YBAYROFF"):
///
/// - WCS: the plate-solve solution, if the source carries one.
/// - Optics/acquisition: the equivalent of what `build_master_cards`
///   (`headers.rs`) derives from its structured `MasterHeaderInputs` —
///   EXPTIME, GAIN, binning, pixel size, INSTRUME, TELESCOP, FOCALLEN,
///   FILTER, … — taken here directly from the raw source cards since a
///   calibrated light has no structured equivalent input.
/// - Session/target: OBJECT, coordinates, site, observer.
/// - Bayer/CFA geometry (mosaic preserved un-debayered per §2).
///
/// Anything not listed here (IMAGETYP, CALSTAT, ATH_* provenance) is either
/// replaced or freshly derived by [`build_light_cal_cards`], never copied
/// blind from the source.
const COPY_THROUGH_KEYWORDS: &[&str] = &[
    // WCS solution
    "CTYPE1", "CTYPE2", "CRVAL1", "CRVAL2", "CRPIX1", "CRPIX2", "CDELT1", "CDELT2", "CD1_1",
    "CD1_2", "CD2_1", "CD2_2", "CROTA1", "CROTA2", "CUNIT1", "CUNIT2", "WCSAXES", "EQUINOX",
    "RADESYS", "LONPOLE", "LATPOLE", "PLTSOLVD", // Optics / acquisition
    "EXPTIME", "GAIN", "OFFSET", "EGAIN", "XBINNING", "YBINNING", "XPIXSZ", "YPIXSZ", "CCD-TEMP",
    "SET-TEMP", "INSTRUME", "TELESCOP", "FOCALLEN", "APTDIA", "FILTER",
    // Observation timestamp
    "DATE-OBS", // Session / target
    "OBJECT", "OBJCTRA", "OBJCTDEC", "RA", "DEC", "SITELAT", "SITELONG", "SITEELEV", "OBSERVER",
    // Bayer / CFA geometry
    "BAYERPAT", "XBAYROFF", "YBAYROFF",
];

/// Inputs specific to one calibrated-light run, not derivable from the
/// source header alone. `dark`/`flat`/`bias` are `(uuid, path)` of the
/// master actually applied — `None` when that type wasn't linked (§2
/// fallback chain; the actually-applied set is reflected in `calstat`).
pub struct LightCalCardInputs {
    pub source_uuid: String,
    pub source_filename: String,
    pub calstat: String,
    pub dark: Option<(String, String)>,
    pub flat: Option<(String, String)>,
    pub bias: Option<(String, String)>,
    pub scale_divisor: f64,
    pub flat_norm_divisor: f64,
}

/// `ATH_CDRK`/`ATH_CFLT`/`ATH_CBIA` value shape: a single Text card holding
/// `"<uuid> <path>"`. Long absolute paths comfortably exceed the FITS
/// fixed-card 68-usable-char limit, but `fits_writer`'s `Card`/`format_card`
/// already implement the FITS 4.0 CONTINUE convention (§4.2.1.2) for
/// arbitrary-length strings — `Card::new` never errors on length, and
/// `format_card` transparently emits a CONTINUE chain. So no truncation is
/// needed here; see `long_master_reference_uses_continue_chain_not_truncation`.
///
/// No trailing comment: the value is arbitrary-length, so a comment on the
/// final continuation record overflows the 80-char card for certain path
/// lengths (`FitsWriteError::CommentTooLong`) and would hard-fail the whole
/// output write. The keyword is self-documenting, so the comment is redundant
/// — dropping it keeps the provenance card robust to any master path length.
fn master_ref_card(
    keyword: &str,
    uuid: &str,
    path: &str,
) -> std::result::Result<Card, FitsWriteError> {
    Card::new(keyword, CardValue::Str(format!("{uuid} {path}")))
}

/// Build the full header card list for a calibrated-light output (§7):
/// copy-through of the source's still-valid metadata, plus CALSTAT and the
/// ATH_C* calibration-provenance cards.
pub fn build_light_cal_cards(
    source_cards: &[Card],
    inputs: &LightCalCardInputs,
) -> Result<Vec<Card>> {
    let mut b = HeaderBuilder::new(FrameKind::Light).calstat(&inputs.calstat);

    for c in source_cards {
        if COPY_THROUGH_KEYWORDS.contains(&c.keyword.as_str()) {
            b = b.custom(c.clone());
        }
    }

    // String-valued provenance cards carry NO comment: their value length is
    // unbounded (a long filename), so a trailing `/ comment` on the final
    // CONTINUE record can overflow the 80-char card and hard-fail the write
    // (`FitsWriteError::CommentTooLong`). The keyword is self-documenting.
    b = b
        .custom(Card::new("ATH_CSRC", CardValue::Str(inputs.source_uuid.clone()))?)
        .custom(Card::new(
            "ATH_CSRN",
            CardValue::Str(inputs.source_filename.clone()),
        )?);

    if let Some((uuid, path)) = &inputs.dark {
        b = b.custom(master_ref_card("ATH_CDRK", uuid, path)?);
    }
    if let Some((uuid, path)) = &inputs.flat {
        b = b.custom(master_ref_card("ATH_CFLT", uuid, path)?);
    }
    if let Some((uuid, path)) = &inputs.bias {
        b = b.custom(master_ref_card("ATH_CBIA", uuid, path)?);
    }

    // Fixed-width numeric provenance cards keep a short comment — the value is
    // right-justified to 20 chars, leaving room for a comment up to ~47 chars,
    // so these must stay well under that (the old ATH_CFNM comment was 51 and
    // overflowed every write).
    b = b
        .custom(
            Card::new("ATH_CSCL", CardValue::Real(inputs.scale_divisor))?
                .with_comment("numeric-scale divisor applied"),
        )
        .custom(
            Card::new("ATH_CFNM", CardValue::Real(inputs.flat_norm_divisor))?
                .with_comment("flat-norm divisor (1.0 = off)"),
        )
        .custom(
            Card::new(
                "ATH_CVER",
                CardValue::Integer(crate::db::light_calibrations::LIGHT_CAL_ENGINE_VERSION),
            )?
            .with_comment("calibration engine version"),
        );

    Ok(b.build()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_inputs() -> LightCalCardInputs {
        LightCalCardInputs {
            source_uuid: "uuid-123".into(),
            source_filename: "L_0001.fits".into(),
            calstat: "BDF".into(),
            dark: Some(("dark-uuid".into(), "/lib/dark.fits".into())),
            flat: Some(("flat-uuid".into(), "/lib/flat.fits".into())),
            bias: None,
            scale_divisor: 65535.0,
            flat_norm_divisor: 1234.5,
        }
    }

    #[test]
    fn cards_contain_calstat_and_identity() {
        let inputs = base_inputs();
        let cards = build_light_cal_cards(&[], &inputs).unwrap();
        let find = |k: &str| cards.iter().find(|c| c.keyword == k);

        let calstat = find("CALSTAT").expect("CALSTAT card");
        assert!(matches!(&calstat.value, Some(CardValue::Str(v)) if v == "BDF"));

        let csrc = find("ATH_CSRC").expect("ATH_CSRC card");
        assert!(matches!(&csrc.value, Some(CardValue::Str(v)) if v == "uuid-123"));

        let csrn = find("ATH_CSRN").expect("ATH_CSRN card");
        assert!(matches!(&csrn.value, Some(CardValue::Str(v)) if v == "L_0001.fits"));

        let dark = find("ATH_CDRK").expect("ATH_CDRK card");
        assert!(matches!(&dark.value, Some(CardValue::Str(v)) if v == "dark-uuid /lib/dark.fits"));

        let flat = find("ATH_CFLT").expect("ATH_CFLT card");
        assert!(matches!(&flat.value, Some(CardValue::Str(v)) if v == "flat-uuid /lib/flat.fits"));

        assert!(
            find("ATH_CBIA").is_none(),
            "no bias applied -> no ATH_CBIA card"
        );

        let scl = find("ATH_CSCL").expect("ATH_CSCL card");
        assert!(matches!(scl.value, Some(CardValue::Real(v)) if (v - 65535.0).abs() < 1e-9));

        let fnm = find("ATH_CFNM").expect("ATH_CFNM card");
        assert!(matches!(fnm.value, Some(CardValue::Real(v)) if (v - 1234.5).abs() < 1e-9));

        let ver = find("ATH_CVER").expect("ATH_CVER card");
        assert!(matches!(
            ver.value,
            Some(CardValue::Integer(v)) if v == crate::db::light_calibrations::LIGHT_CAL_ENGINE_VERSION
        ));

        // Every new keyword respects the 8-char FITS limit.
        for kw in [
            "CALSTAT", "ATH_CSRC", "ATH_CSRN", "ATH_CDRK", "ATH_CFLT", "ATH_CBIA", "ATH_CSCL",
            "ATH_CFNM", "ATH_CVER",
        ] {
            assert!(kw.len() <= 8, "{kw}");
        }
    }

    #[test]
    fn bayer_cards_copied_through() {
        let source_cards = vec![
            Card::new("BAYERPAT", CardValue::Str("RGGB".into())).unwrap(),
            Card::new("XBAYROFF", CardValue::Integer(0)).unwrap(),
            Card::new("YBAYROFF", CardValue::Integer(1)).unwrap(),
            // Not in the copy-through whitelist — must NOT be forwarded.
            Card::new("SWCREATE", CardValue::Str("some other app".into())).unwrap(),
        ];
        let inputs = base_inputs();
        let cards = build_light_cal_cards(&source_cards, &inputs).unwrap();
        let find = |k: &str| cards.iter().find(|c| c.keyword == k);

        let bp = find("BAYERPAT").expect("BAYERPAT copied through");
        assert!(matches!(&bp.value, Some(CardValue::Str(v)) if v == "RGGB"));
        assert!(find("XBAYROFF").is_some(), "XBAYROFF copied through");
        assert!(find("YBAYROFF").is_some(), "YBAYROFF copied through");
        assert!(
            find("SWCREATE").is_none(),
            "SWCREATE is not in the copy-through whitelist"
        );
    }

    #[test]
    fn flat_norm_divisor_1_when_disabled() {
        let mut inputs = base_inputs();
        inputs.flat_norm_divisor = 1.0; // normalization off (§2)
        let cards = build_light_cal_cards(&[], &inputs).unwrap();
        let fnm = cards
            .iter()
            .find(|c| c.keyword == "ATH_CFNM")
            .expect("ATH_CFNM card");
        assert!(matches!(fnm.value, Some(CardValue::Real(v)) if (v - 1.0).abs() < 1e-9));
    }

    #[test]
    fn long_master_reference_uses_continue_chain_not_truncation() {
        // uuid (36 chars) + space + a long absolute path comfortably
        // exceeds the 68-usable-char single-card limit.
        let uuid = "01234567-89ab-cdef-0123-456789abcdef";
        let long_path = format!("/Volumes/Archive/{}/master_dark.fits", "sub/".repeat(20));
        assert!(
            uuid.len() + 1 + long_path.len() > 68,
            "fixture must actually exceed the limit"
        );

        let mut inputs = base_inputs();
        inputs.dark = Some((uuid.to_string(), long_path.clone()));
        let cards = build_light_cal_cards(&[], &inputs).unwrap();
        let dark = cards
            .iter()
            .find(|c| c.keyword == "ATH_CDRK")
            .expect("ATH_CDRK card");

        let expected = format!("{uuid} {long_path}");
        assert!(
            matches!(&dark.value, Some(CardValue::Str(v)) if v == &expected),
            "value must be the full, untruncated uuid+path: {:?}",
            dark.value
        );

        // Confirm the writer itself handles the >68-char content via a
        // CONTINUE chain rather than erroring (fits_writer::card::format_card).
        let records = crate::fits_writer::card::format_card(dark)
            .expect("format_card must not error on a long string value");
        assert!(
            records.len() > 1,
            "expected a multi-record CONTINUE chain, got {}",
            records.len()
        );
    }

    /// Regression: a master-reference card must render for ANY master-path
    /// length. Before dropping the decorative comment from `master_ref_card`, a
    /// path whose length landed the CONTINUE chain's final chunk near the 80-
    /// char card boundary made the trailing `/ comment` overflow, so
    /// `format_card` returned `CommentTooLong` and the whole calibrated-light
    /// write hard-failed. This swept-length loop pins that these cards never
    /// carry an overflowing comment and always format cleanly.
    #[test]
    fn master_reference_cards_format_at_every_path_length() {
        let uuid = "01234567-89ab-cdef-0123-456789abcdef"; // 36 chars, as real
        for extra in 0..80usize {
            let path = format!("/lib/{}/master_dark.fits", "a".repeat(extra));
            let mut inputs = base_inputs();
            inputs.dark = Some((uuid.to_string(), path.clone()));
            inputs.flat = None;
            inputs.bias = None;

            let cards = build_light_cal_cards(&[], &inputs).unwrap();
            let dark = cards
                .iter()
                .find(|c| c.keyword == "ATH_CDRK")
                .expect("ATH_CDRK card");
            assert!(dark.comment.is_none(), "master-ref card must carry no comment");
            // Every card in the built header must render — the write path formats
            // them all, so a single overflowing comment fails the whole output.
            for c in &cards {
                crate::fits_writer::card::format_card(c).unwrap_or_else(|e| {
                    panic!(
                        "format_card failed for {} at path len {}: {e:?}",
                        c.keyword,
                        path.len()
                    )
                });
            }
        }
    }

    /// The always-present fixed provenance cards (independent of any path
    /// length) must render — ATH_CFNM in particular carried a 51-char comment
    /// that overflowed every write before it was shortened.
    #[test]
    fn fixed_provenance_cards_never_overflow() {
        let inputs = base_inputs();
        let cards = build_light_cal_cards(&[], &inputs).unwrap();
        for kw in ["ATH_CSRC", "ATH_CSRN", "ATH_CSCL", "ATH_CFNM", "ATH_CVER"] {
            let card = cards.iter().find(|c| c.keyword == kw).unwrap();
            crate::fits_writer::card::format_card(card)
                .unwrap_or_else(|e| panic!("{kw} must format cleanly: {e:?}"));
        }
    }
}

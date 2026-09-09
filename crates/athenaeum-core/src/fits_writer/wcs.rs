//! WCS/SIP header cards derived from a stored plate solve.
//!
//! Turns a `plate_solve::storage::PlateSolveRecord` into standard TAN(-SIP)
//! WCS header cards: linear astrometry via `CRPIX`/`CRVAL`/`CD`, plus SIP
//! distortion polynomials when the record carries them. `PlateSolveRecord`
//! stores `crpix1`/`crpix2` 0-based (the solver's own convention); the FITS
//! `CRPIX1`/`CRPIX2` cards are 1-based, so `crpix + 1.0`. SIP tables are
//! stored as triangular JSON (`coeffs[i][j]` the coefficient of `u^i v^j`,
//! `i + j <= order`) — the constant and linear terms (`i + j < 2`) belong to
//! the CD matrix and are never written as SIP cards.

use crate::fits_writer::{Card, CardValue, FitsWriteError};
use crate::plate_solve::storage::PlateSolveRecord;

/// Parse one stored SIP coefficient table. `name` identifies the table in
/// error messages (`"A"`, `"B"`, `"AP"`, `"BP"`).
fn parse_sip_table(name: &str, json: &str) -> Result<Vec<Vec<f64>>, FitsWriteError> {
    serde_json::from_str::<Vec<Vec<f64>>>(json).map_err(|e| {
        FitsWriteError::ValueTooLong(format!("SIP table {name} is not valid JSON: {e}"))
    })
}

/// Emit `{prefix}_ORDER` plus one `{prefix}_{i}_{j}` card per non-zero
/// coefficient with `i + j >= 2`. A non-zero constant or linear term
/// (`i + j < 2`) is a malformed table — those terms are folded into the CD
/// matrix by convention and must be zero in the stored SIP table.
fn push_sip_cards(
    prefix: &str,
    order: i32,
    table: &[Vec<f64>],
    out: &mut Vec<Card>,
) -> Result<(), FitsWriteError> {
    out.push(Card::new(
        &format!("{prefix}_ORDER"),
        CardValue::Integer(i64::from(order)),
    )?);
    for (i, row) in table.iter().enumerate() {
        for (j, &v) in row.iter().enumerate() {
            if i + j < 2 {
                if v != 0.0 {
                    return Err(FitsWriteError::ValueTooLong(format!(
                        "{prefix}_{i}_{j} is a constant/linear SIP term (must be 0.0 — it belongs to the CD matrix), got {v}"
                    )));
                }
                continue;
            }
            if v == 0.0 {
                continue; // zero coefficients are not written
            }
            out.push(Card::new(&format!("{prefix}_{i}_{j}"), CardValue::Real(v))?);
        }
    }
    Ok(())
}

/// Build WCS/SIP header cards from a stored plate solve, ready to pass to
/// `fits_writer::write_fits_f32`. Returns an error rather than a silently
/// linear-only header when the record's SIP tables are present but
/// malformed (bad JSON, or a non-zero term the CD matrix already owns).
pub fn wcs_cards(solve: &PlateSolveRecord) -> Result<Vec<Card>, FitsWriteError> {
    let has_sip = solve.sip_order.is_some();
    if has_sip && (solve.sip_a_coeffs.is_none() || solve.sip_b_coeffs.is_none()) {
        return Err(FitsWriteError::ValueTooLong(
            "plate solve record has a SIP order but is missing the A/B coefficient table"
                .to_string(),
        ));
    }

    let mut cards = Vec::new();

    cards.push(Card::new("WCSAXES", CardValue::Integer(2))?);

    let suffix = if has_sip { "-SIP" } else { "" };
    cards.push(Card::new(
        "CTYPE1",
        CardValue::Str(format!("RA---TAN{suffix}")),
    )?);
    cards.push(Card::new(
        "CTYPE2",
        CardValue::Str(format!("DEC--TAN{suffix}")),
    )?);

    cards.push(
        Card::new("CRPIX1", CardValue::Real(solve.crpix1 + 1.0))?
            .with_comment("1-based reference pixel"),
    );
    cards.push(
        Card::new("CRPIX2", CardValue::Real(solve.crpix2 + 1.0))?
            .with_comment("1-based reference pixel"),
    );

    cards.push(Card::new("CRVAL1", CardValue::Real(solve.crval1))?.with_comment("deg"));
    cards.push(Card::new("CRVAL2", CardValue::Real(solve.crval2))?.with_comment("deg"));

    cards.push(Card::new("CD1_1", CardValue::Real(solve.cd1_1))?);
    cards.push(Card::new("CD1_2", CardValue::Real(solve.cd1_2))?);
    cards.push(Card::new("CD2_1", CardValue::Real(solve.cd2_1))?);
    cards.push(Card::new("CD2_2", CardValue::Real(solve.cd2_2))?);

    cards.push(Card::new("CUNIT1", CardValue::Str("deg".to_string()))?);
    cards.push(Card::new("CUNIT2", CardValue::Str("deg".to_string()))?);

    cards.push(Card::new("RADESYS", CardValue::Str("ICRS".to_string()))?);
    cards.push(Card::new("EQUINOX", CardValue::Real(2000.0))?);

    if has_sip {
        // Presence checked above: sip_order, sip_a_coeffs and sip_b_coeffs are all Some.
        let order = solve
            .sip_order
            .expect("has_sip checked sip_order.is_some() above");
        let a = parse_sip_table(
            "A",
            solve
                .sip_a_coeffs
                .as_deref()
                .expect("has_sip checked sip_a_coeffs above"),
        )?;
        let b = parse_sip_table(
            "B",
            solve
                .sip_b_coeffs
                .as_deref()
                .expect("has_sip checked sip_b_coeffs above"),
        )?;
        push_sip_cards("A", order, &a, &mut cards)?;
        push_sip_cards("B", order, &b, &mut cards)?;

        // Reverse (AP/BP) polynomials are only meaningful as a pair — a
        // record with just one of them is treated as having no reverse
        // solution rather than an error.
        if let (Some(ap_json), Some(bp_json)) = (
            solve.sip_ap_coeffs.as_deref(),
            solve.sip_bp_coeffs.as_deref(),
        ) {
            let ap = parse_sip_table("AP", ap_json)?;
            let bp = parse_sip_table("BP", bp_json)?;
            push_sip_cards("AP", order, &ap, &mut cards)?;
            push_sip_cards("BP", order, &bp, &mut cards)?;
        }
    }

    cards.push(
        Card::new("PLTSOLVD", CardValue::Logical(true))?.with_comment("plate solved by Athenaeum"),
    );

    Ok(cards)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plate_solve::storage::PlateSolveRecord;

    fn record(sip: bool) -> PlateSolveRecord {
        PlateSolveRecord {
            id: None,
            frame_id: 1,
            crpix1: 3111.5,
            crpix2: 2083.5, // 0-based
            crval1: 316.25,
            crval2: 70.45,
            cd1_1: -2.5e-4,
            cd1_2: 1.0e-6,
            cd2_1: -1.0e-6,
            cd2_2: -2.5e-4,
            sip_order: sip.then_some(2),
            sip_a_coeffs: sip.then(|| "[[0.0,0.0,1.0e-7],[0.0,2.0e-7],[3.0e-7]]".to_string()),
            sip_b_coeffs: sip.then(|| "[[0.0,0.0,-1.0e-7],[0.0,-2.0e-7],[-3.0e-7]]".to_string()),
            sip_ap_coeffs: None,
            sip_bp_coeffs: None,
            matched_stars: 100,
            total_detected: 200,
            rms_residual_px: 0.3,
            rms_residual_arcsec: 0.27,
            pixel_scale_arcsec: 0.9,
            field_rotation_deg: 0.0,
            solve_time_ms: 10,
            catalog_used: "test".into(),
            algorithm_used: "test".into(),
            solved_at: "2026-09-09T00:00:00Z".into(),
            expected_catalog_stars_in_fov: None,
            inlier_ratio: None,
        }
    }

    fn value<'a>(cards: &'a [Card], kw: &str) -> &'a CardValue {
        cards
            .iter()
            .find(|c| c.keyword == kw)
            .unwrap_or_else(|| panic!("no {kw}"))
            .value
            .as_ref()
            .unwrap()
    }

    #[test]
    fn linear_wcs_cards_are_one_based_and_tan() {
        let cards = wcs_cards(&record(false)).unwrap();
        assert_eq!(value(&cards, "CTYPE1"), &CardValue::Str("RA---TAN".into()));
        assert_eq!(value(&cards, "CTYPE2"), &CardValue::Str("DEC--TAN".into()));
        assert_eq!(value(&cards, "CRPIX1"), &CardValue::Real(3112.5));
        assert_eq!(value(&cards, "CRPIX2"), &CardValue::Real(2084.5));
        assert_eq!(value(&cards, "CRVAL1"), &CardValue::Real(316.25));
        assert_eq!(value(&cards, "CD1_1"), &CardValue::Real(-2.5e-4));
        assert_eq!(value(&cards, "CD2_2"), &CardValue::Real(-2.5e-4));
        assert_eq!(value(&cards, "CUNIT1"), &CardValue::Str("deg".into()));
        assert_eq!(value(&cards, "RADESYS"), &CardValue::Str("ICRS".into()));
        assert_eq!(value(&cards, "EQUINOX"), &CardValue::Real(2000.0));
        assert_eq!(value(&cards, "WCSAXES"), &CardValue::Integer(2));
        assert!(cards
            .iter()
            .all(|c| !c.keyword.starts_with("A_") && c.keyword != "A_ORDER"));
    }

    #[test]
    fn sip_cards_follow_the_stored_triangular_table() {
        let cards = wcs_cards(&record(true)).unwrap();
        assert_eq!(
            value(&cards, "CTYPE1"),
            &CardValue::Str("RA---TAN-SIP".into())
        );
        assert_eq!(value(&cards, "A_ORDER"), &CardValue::Integer(2));
        assert_eq!(value(&cards, "B_ORDER"), &CardValue::Integer(2));
        assert_eq!(value(&cards, "A_0_2"), &CardValue::Real(1.0e-7));
        assert_eq!(value(&cards, "A_1_1"), &CardValue::Real(2.0e-7));
        assert_eq!(value(&cards, "A_2_0"), &CardValue::Real(3.0e-7));
        assert_eq!(value(&cards, "B_2_0"), &CardValue::Real(-3.0e-7));
        // zero coefficients are not written; no AP/BP without a reverse solution
        assert!(cards
            .iter()
            .all(|c| c.keyword != "A_0_0" && c.keyword != "A_0_1"));
        assert!(cards
            .iter()
            .all(|c| !c.keyword.starts_with("AP_") && !c.keyword.starts_with("BP_")));
    }

    #[test]
    fn malformed_sip_json_is_an_error_not_a_silent_linear_header() {
        let mut r = record(true);
        r.sip_a_coeffs = Some("not json".into());
        assert!(wcs_cards(&r).is_err());
    }

    #[test]
    fn cards_round_trip_through_the_header_reader() {
        let cards = wcs_cards(&record(true)).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wcs_roundtrip.fits");
        let data = vec![0.0f32; 4];
        crate::fits_writer::write_fits_f32(&path, 2, 2, 1, &data, &cards).unwrap();

        let header = crate::fits_parser::FitsHeader::from_path(&path).unwrap();
        assert_eq!(header.get_f64("CRPIX1"), Some(3112.5));
        assert_eq!(header.get_str("CTYPE1"), Some("RA---TAN-SIP".to_string()));
        let a20 = header.get_f64("A_2_0").expect("A_2_0 card");
        assert!((a20 - 3.0e-7).abs() < 1e-12, "got {a20}");
    }
}

//! WCS and SIP cards from a stored plate solve (`PlateSolveRecord`).
//! `CRPIX` on the card is the record's 0-based reference pixel + 1. SIP
//! tables are the solver's square `(order+1)×(order+1)` JSON arrays with
//! `coeffs[i][j]` the coefficient of `u^i v^j`; every non-zero term with
//! `i + j ≤ order` is written, INCLUDING the constant and linear ones the
//! solver fits into the table — readers evaluate the full array, so the
//! header reproduces the solver's `pixel_to_sky` exactly. Folding those
//! terms into CD/CRPIX (a Shupe-conformant header) is not done here.

use crate::fits_writer::{Card, CardValue, FitsWriteError};
use crate::plate_solve::storage::PlateSolveRecord;

/// Parse one stored SIP coefficient table. `name` identifies the table in
/// error messages (`"A"`, `"B"`, `"AP"`, `"BP"`).
fn parse_sip_table(name: &str, json: &str) -> Result<Vec<Vec<f64>>, FitsWriteError> {
    serde_json::from_str::<Vec<Vec<f64>>>(json)
        .map_err(|e| FitsWriteError::Malformed(format!("SIP table {name} is not valid JSON: {e}")))
}

/// Derive a SIP table's order from its own row count (a square table of `n`
/// rows has order `n - 1`) and check it against the record's declared
/// `sip_order` — the two must agree, or the header would either drop real
/// terms or claim terms that were never fit.
fn table_order(name: &str, table: &[Vec<f64>], declared: i32) -> Result<usize, FitsWriteError> {
    let derived = table.len().saturating_sub(1);
    if derived != declared as usize {
        return Err(FitsWriteError::Malformed(format!(
            "SIP table {name} has {} rows, order {declared} declared",
            table.len()
        )));
    }
    Ok(derived)
}

/// Emit `{prefix}_ORDER` plus one `{prefix}_{i}_{j}` card per non-zero
/// coefficient with `i + j <= order`. This includes the constant (`0,0`)
/// and linear (`1,0`/`0,1`) terms the solver absorbs into the table — the
/// written header must reproduce the solver's own polynomial evaluation, so
/// nothing is folded into the CD matrix here. A non-zero low-order term is
/// logged once per table (it means this header is not Shupe-conformant),
/// never rejected.
fn push_sip_cards(
    prefix: &str,
    order: usize,
    table: &[Vec<f64>],
    out: &mut Vec<Card>,
) -> Result<(), FitsWriteError> {
    out.push(Card::new(
        &format!("{prefix}_ORDER"),
        CardValue::Integer(order as i64),
    )?);
    let mut warned_low_order = false;
    for (i, row) in table.iter().enumerate() {
        for (j, &v) in row.iter().enumerate() {
            if i + j > order {
                continue; // outside the fitted polynomial's terms
            }
            if v == 0.0 {
                continue; // zero coefficients are not written
            }
            if i + j < 2 && v.abs() > 1e-12 && !warned_low_order {
                tracing::warn!(
                    table = prefix,
                    i,
                    j,
                    value = v,
                    "SIP low-order term written; CD fold not applied"
                );
                warned_low_order = true;
            }
            out.push(Card::new(&format!("{prefix}_{i}_{j}"), CardValue::Real(v))?);
        }
    }
    Ok(())
}

/// Output-grid coordinate of a reference coordinate for an integer drizzle
/// scale, pixel-centres convention: `v = s·u + (s − 1)/2` (reference pixel
/// `i` covers output pixels `s·i .. s·i + s − 1`). This is the SAME formula
/// as `stacking::drizzle::geom::to_output`, duplicated rather than called:
/// `fits_writer` is unconditionally compiled while `stacking` is gated
/// behind the `render`+`solver` features, so a direct call here would break
/// `cargo check -p athenaeum-core --no-default-features`.
#[inline]
fn scaled_crpix(u: f64, s: f64) -> f64 {
    s * u + (s - 1.0) / 2.0
}

/// Scale one stored SIP coefficient table for drizzle: `A'_pq = A_pq ·
/// s^(1 − p − q)` for every stored `(p, q) = (i, j)` term (`scale_plate_solve`).
/// Decodes with [`parse_sip_table`] (the same helper `wcs_cards` uses), so a
/// malformed table is the same [`FitsWriteError::Malformed`] here, never a
/// silent linear-only result. `None` stays `None`.
fn scale_sip_table_opt(
    name: &str,
    json: Option<&str>,
    s: f64,
) -> Result<Option<String>, FitsWriteError> {
    let Some(json) = json else {
        return Ok(None);
    };
    let table = parse_sip_table(name, json)?;
    let scaled: Vec<Vec<f64>> = table
        .iter()
        .enumerate()
        .map(|(i, row)| {
            row.iter()
                .enumerate()
                .map(|(j, &v)| v * s.powi(1 - i as i32 - j as i32))
                .collect()
        })
        .collect();
    let json = serde_json::to_string(&scaled).map_err(|e| {
        FitsWriteError::Malformed(format!(
            "SIP table {name} failed to re-encode after drizzle scaling: {e}"
        ))
    })?;
    Ok(Some(json))
}

/// The same solve on an image resampled by an integer factor `s` about the
/// pixel grid (drizzle, R-M3-13). `crpix1`/`crpix2` are 0-based (the card
/// writer adds 1), so they scale with [`scaled_crpix`]: `crpix' = s·crpix +
/// (s − 1)/2` (a reference pixel's centre lands between its s×s output
/// pixels; on the 1-based `CRPIX` card this reads `s·CRPIX − (s − 1)/2`).
/// `CD' = CD / s` (every element), `pixel_scale' = pixel_scale / s`,
/// `rms_residual_px' = rms_residual_px · s` (the same residual, now
/// measured in output pixels). SIP `A'_pq = A_pq · s^(1 − p − q)` (same for
/// B; AP/BP when present) — see [`scale_sip_table_opt`]. `crval*`,
/// `field_rotation_deg`, `matched_stars`, `id`/`frame_id` and everything
/// else not named above is copied unchanged. `s = 1` returns an unchanged
/// clone (no re-encoding round-trip of the SIP JSON either).
///
/// Returns `Result` rather than the plain `PlateSolveRecord` a first read of
/// the SIP scaling rule might suggest: a malformed stored SIP table is an
/// error under the same contract [`wcs_cards`] enforces, never silently
/// dropped or left linear-only.
pub fn scale_plate_solve(
    solve: &PlateSolveRecord,
    s: u32,
) -> Result<PlateSolveRecord, FitsWriteError> {
    if s == 1 {
        return Ok(solve.clone());
    }
    let sf = s as f64;
    let mut out = solve.clone();
    out.crpix1 = scaled_crpix(solve.crpix1, sf);
    out.crpix2 = scaled_crpix(solve.crpix2, sf);
    out.cd1_1 = solve.cd1_1 / sf;
    out.cd1_2 = solve.cd1_2 / sf;
    out.cd2_1 = solve.cd2_1 / sf;
    out.cd2_2 = solve.cd2_2 / sf;
    out.pixel_scale_arcsec = solve.pixel_scale_arcsec / sf;
    out.rms_residual_px = solve.rms_residual_px * sf;

    out.sip_a_coeffs = scale_sip_table_opt("A", solve.sip_a_coeffs.as_deref(), sf)?;
    out.sip_b_coeffs = scale_sip_table_opt("B", solve.sip_b_coeffs.as_deref(), sf)?;
    out.sip_ap_coeffs = scale_sip_table_opt("AP", solve.sip_ap_coeffs.as_deref(), sf)?;
    out.sip_bp_coeffs = scale_sip_table_opt("BP", solve.sip_bp_coeffs.as_deref(), sf)?;

    Ok(out)
}

/// Build WCS/SIP header cards from a stored plate solve, ready to pass to
/// `fits_writer::write_fits_f32`. Returns an error rather than a silently
/// linear-only header when the record's SIP tables are present but
/// malformed (bad JSON, or a table whose size disagrees with the declared
/// order).
pub fn wcs_cards(solve: &PlateSolveRecord) -> Result<Vec<Card>, FitsWriteError> {
    let has_sip = solve.sip_order.is_some();
    if has_sip && (solve.sip_a_coeffs.is_none() || solve.sip_b_coeffs.is_none()) {
        return Err(FitsWriteError::Malformed(
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
    cards.push(
        Card::new("EQUINOX", CardValue::Real(2000.0))?
            .with_comment("FK5 equinox; informational under ICRS"),
    );

    if has_sip {
        // Presence checked above: sip_order, sip_a_coeffs and sip_b_coeffs are all Some.
        let declared = solve
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
        let a_order = table_order("A", &a, declared)?;
        let b_order = table_order("B", &b, declared)?;
        push_sip_cards("A", a_order, &a, &mut cards)?;
        push_sip_cards("B", b_order, &b, &mut cards)?;

        // Reverse (AP/BP) polynomials are only meaningful as a pair — a
        // record with just one of them is treated as having no reverse
        // solution rather than an error.
        if let (Some(ap_json), Some(bp_json)) = (
            solve.sip_ap_coeffs.as_deref(),
            solve.sip_bp_coeffs.as_deref(),
        ) {
            let ap = parse_sip_table("AP", ap_json)?;
            let bp = parse_sip_table("BP", bp_json)?;
            let ap_order = table_order("AP", &ap, declared)?;
            let bp_order = table_order("BP", &bp, declared)?;
            push_sip_cards("AP", ap_order, &ap, &mut cards)?;
            push_sip_cards("BP", bp_order, &bp, &mut cards)?;
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

    // Solver-shaped: square (order+1)x(order+1) tables (solvemyastro::sip::vec_to_sip
    // fits the full basis from p=0, so the constant/linear terms are real and
    // deliberately non-zero — see the module doc above).
    const SIP_A: &str = "[[1.0e-6,2.0e-6,1.0e-7],[3.0e-6,2.0e-7,0.0],[3.0e-7,0.0,0.0]]";
    const SIP_B: &str = "[[-1.0e-6,-2.0e-6,-1.0e-7],[-3.0e-6,-2.0e-7,0.0],[-3.0e-7,0.0,0.0]]";

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
            sip_a_coeffs: sip.then(|| SIP_A.to_string()),
            sip_b_coeffs: sip.then(|| SIP_B.to_string()),
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
    fn sip_cards_follow_the_stored_square_table() {
        let cards = wcs_cards(&record(true)).unwrap();
        assert_eq!(
            value(&cards, "CTYPE1"),
            &CardValue::Str("RA---TAN-SIP".into())
        );
        assert_eq!(value(&cards, "A_ORDER"), &CardValue::Integer(2));
        assert_eq!(value(&cards, "A_0_0"), &CardValue::Real(1.0e-6));
        assert_eq!(value(&cards, "A_1_0"), &CardValue::Real(3.0e-6));
        assert_eq!(value(&cards, "A_0_1"), &CardValue::Real(2.0e-6));
        assert_eq!(value(&cards, "A_0_2"), &CardValue::Real(1.0e-7));
        assert_eq!(value(&cards, "A_1_1"), &CardValue::Real(2.0e-7));
        assert_eq!(value(&cards, "A_2_0"), &CardValue::Real(3.0e-7));
        assert_eq!(value(&cards, "B_2_0"), &CardValue::Real(-3.0e-7));
        // zero and/or beyond-the-order terms are not written; no AP/BP without a reverse solution
        assert!(cards
            .iter()
            .all(|c| c.keyword != "A_2_1" && c.keyword != "A_1_2" && c.keyword != "A_2_2"));
        assert!(cards
            .iter()
            .all(|c| !c.keyword.starts_with("AP_") && !c.keyword.starts_with("BP_")));
    }

    #[test]
    fn reverse_tables_get_their_own_order_cards() {
        let mut r = record(true);
        r.sip_ap_coeffs = Some(SIP_A.to_string());
        r.sip_bp_coeffs = Some(SIP_B.to_string());
        let cards = wcs_cards(&r).unwrap();
        assert_eq!(value(&cards, "AP_ORDER"), &CardValue::Integer(2));
        assert_eq!(value(&cards, "BP_ORDER"), &CardValue::Integer(2));
        assert_eq!(value(&cards, "AP_1_0"), &CardValue::Real(3.0e-6));
        assert_eq!(value(&cards, "BP_2_0"), &CardValue::Real(-3.0e-7));
    }

    #[test]
    fn a_table_whose_size_disagrees_with_the_declared_order_is_rejected() {
        let mut r = record(true);
        r.sip_order = Some(3); // SIP_A/SIP_B are 3-row (order 2) tables
        assert!(wcs_cards(&r).is_err());
    }

    #[test]
    fn malformed_sip_json_is_an_error_not_a_silent_linear_header() {
        let mut r = record(true);
        r.sip_a_coeffs = Some("not json".into());
        let err = wcs_cards(&r).unwrap_err();
        assert!(
            err.to_string().starts_with("malformed FITS input:"),
            "got {err}"
        );
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
        assert_eq!(header.get_f64("A_ORDER"), Some(2.0));
        assert_eq!(header.get_f64("CD1_1"), Some(-2.5e-4));
        let a10 = header.get_f64("A_1_0").expect("A_1_0 card");
        assert!((a10 - 3.0e-6).abs() < 1e-12, "got {a10}");
        let a20 = header.get_f64("A_2_0").expect("A_2_0 card");
        assert!((a20 - 3.0e-7).abs() < 1e-12, "got {a20}");
    }

    // Drizzle SIP tables (M3 Task 4, R-M3-13): order-2, square (order+1)x(order+1).
    // Only A_2_0/A_1_1/A_0_2 (the order-2 terms, all sharing exponent
    // s^(1-2)=s^-1) are asserted below — the pinned values are the brief's
    // own worked example. B is present so `has_sip` stays satisfied by a
    // real record but its scaled values are not separately asserted here.
    const DRIZZLE_SIP_A: &str = "[[0.0,0.0,3.0e-6],[0.0,2.0e-6,0.0],[1.0e-6,0.0,0.0]]";
    const DRIZZLE_SIP_B: &str = "[[0.0,0.0,-3.0e-6],[0.0,-2.0e-6,0.0],[-1.0e-6,0.0,0.0]]";

    fn drizzle_record() -> PlateSolveRecord {
        PlateSolveRecord {
            id: Some(9),
            frame_id: 42,
            crpix1: 100.0,
            crpix2: 50.0,
            crval1: 200.0,
            crval2: -10.0,
            cd1_1: 1.0e-4,
            cd1_2: 0.0,
            cd2_1: 0.0,
            cd2_2: -1.0e-4,
            sip_order: Some(2),
            sip_a_coeffs: Some(DRIZZLE_SIP_A.to_string()),
            sip_b_coeffs: Some(DRIZZLE_SIP_B.to_string()),
            sip_ap_coeffs: None,
            sip_bp_coeffs: None,
            matched_stars: 50,
            total_detected: 80,
            rms_residual_px: 0.4,
            rms_residual_arcsec: 0.36,
            pixel_scale_arcsec: 1.2,
            field_rotation_deg: 3.5,
            solve_time_ms: 5,
            catalog_used: "test".into(),
            algorithm_used: "test".into(),
            solved_at: "2026-09-10T00:00:00Z".into(),
            expected_catalog_stars_in_fov: Some(60),
            inlier_ratio: Some(0.8),
        }
    }

    fn sip_table(json: &str) -> Vec<Vec<f64>> {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn scale_plate_solve_scales_crpix_cd_pixel_scale_rms_and_sip() {
        let solve = drizzle_record();
        let scaled = scale_plate_solve(&solve, 2).unwrap();

        // crpix' = s*crpix + (s-1)/2 = 2*100 + 0.5 = 200.5 (2*101 - 0.5 == 201.5 on the 1-based card)
        assert_eq!(scaled.crpix1, 200.5);
        assert_eq!(scaled.crpix2, 100.5);
        let cards = wcs_cards(&scaled).unwrap();
        assert_eq!(value(&cards, "CRPIX1"), &CardValue::Real(201.5));

        assert_eq!(scaled.cd1_1, 5.0e-5);
        assert_eq!(scaled.cd2_2, -5.0e-5);
        assert_eq!(scaled.cd1_2, 0.0);
        assert_eq!(scaled.cd2_1, 0.0);

        assert!((scaled.pixel_scale_arcsec - 0.6).abs() < 1e-12);
        assert!((scaled.rms_residual_px - 0.8).abs() < 1e-12);

        // untouched fields, copied through
        assert_eq!(scaled.id, solve.id);
        assert_eq!(scaled.frame_id, solve.frame_id);
        assert_eq!(scaled.crval1, solve.crval1);
        assert_eq!(scaled.crval2, solve.crval2);
        assert_eq!(scaled.field_rotation_deg, solve.field_rotation_deg);
        assert_eq!(scaled.matched_stars, solve.matched_stars);
        assert_eq!(scaled.rms_residual_arcsec, solve.rms_residual_arcsec);

        let a = sip_table(scaled.sip_a_coeffs.as_deref().unwrap());
        assert!((a[2][0] - 5.0e-7).abs() < 1e-15, "A_2_0: {}", a[2][0]);
        assert!((a[1][1] - 1.0e-6).abs() < 1e-15, "A_1_1: {}", a[1][1]);
        assert!((a[0][2] - 1.5e-6).abs() < 1e-15, "A_0_2: {}", a[0][2]);

        // s = 1 is an unchanged clone (byte-identical SIP JSON too, no
        // decode/re-encode round-trip drift).
        let unscaled = scale_plate_solve(&solve, 1).unwrap();
        assert_eq!(
            serde_json::to_value(&solve).unwrap(),
            serde_json::to_value(&unscaled).unwrap()
        );
    }

    #[test]
    fn scale_plate_solve_rejects_a_malformed_sip_table() {
        let mut solve = drizzle_record();
        solve.sip_a_coeffs = Some("not json".into());
        let err = scale_plate_solve(&solve, 2).unwrap_err();
        assert!(
            err.to_string().starts_with("malformed FITS input:"),
            "got {err}"
        );
    }

    /// Fallback per the task brief: no pixel->sky evaluator exists in this
    /// crate, so this asserts the linear part directly — `CD'·(p' −
    /// CRPIX') == CD·(p − CRPIX)` for three pixels, where `p'` is the
    /// output-grid coordinate of `p` (`stacking::drizzle::geom::to_output`,
    /// the SAME formula `scaled_crpix` duplicates above). A record with no
    /// SIP is used since the SIP correction is not part of this identity.
    #[test]
    fn scaled_solve_preserves_the_linear_pixel_to_sky_map() {
        use crate::stacking::drizzle::geom::to_output;

        let solve = record(false);
        let s = 2u32;
        let scaled = scale_plate_solve(&solve, s).unwrap();

        for (px, py) in [(0.0, 0.0), (500.0, 300.0), (-200.0, 4000.0)] {
            let dx = px - solve.crpix1;
            let dy = py - solve.crpix2;
            let orig = (
                solve.cd1_1 * dx + solve.cd1_2 * dy,
                solve.cd2_1 * dx + solve.cd2_2 * dy,
            );

            let opx = to_output(px, s);
            let opy = to_output(py, s);
            let sdx = opx - scaled.crpix1;
            let sdy = opy - scaled.crpix2;
            let scaled_delta = (
                scaled.cd1_1 * sdx + scaled.cd1_2 * sdy,
                scaled.cd2_1 * sdx + scaled.cd2_2 * sdy,
            );

            assert!(
                (scaled_delta.0 - orig.0).abs() < 1e-12,
                "x: {scaled_delta:?} vs {orig:?}"
            );
            assert!(
                (scaled_delta.1 - orig.1).abs() < 1e-12,
                "y: {scaled_delta:?} vs {orig:?}"
            );
        }
    }
}

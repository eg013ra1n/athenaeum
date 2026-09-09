//! Master-light header (spec §6.4), file naming (spec §9.5) and the writers
//! for the master and its two optional rejection maps. Consumes a group's
//! `GroupOutput` (Plan 4's `integrate::integrate_group`) plus the reference
//! frame's own copy-through cards (`register::writer::source_cards_from_file`)
//! and, when the reference has one, its plate solve — the WCS card writer
//! (`fits_writer::wcs::wcs_cards`) reproduces that solve unchanged since the
//! master shares the reference's pixel grid.

use std::path::{Path, PathBuf};

use anyhow::Context;

use crate::archive::path_layout::sanitize_for_filename;
use crate::calibration_library::paths::resolve_collision;
use crate::fits_writer::keywords::{FrameKind, HeaderBuilder};
use crate::fits_writer::wcs::wcs_cards;
use crate::fits_writer::{write_fits_f32, Card, CardValue, FitsWriteError};
use crate::plate_solve::storage::PlateSolveRecord;
use crate::stacking::integrate::GroupOutput;
use crate::stacking::register::writer::REGISTERED_COPY_THROUGH;

/// `ATH_STKV`: the master-light header format version.
pub const ATH_STK_VERSION: i64 = 1;

pub struct MasterCardInputs<'a> {
    /// Copy-through cards of the REFERENCE frame (`source_cards_from_file`).
    pub reference_cards: &'a [Card],
    /// The reference's plate solve, when it has one.
    pub wcs: Option<&'a PlateSolveRecord>,
    pub frames: usize,
    pub weighted_exposure_s: f64,
    /// Earliest DATE-OBS among the included frames; when None the
    /// reference's own DATE-OBS card is kept.
    pub date_obs_first: Option<&'a str>,
    /// Latest DATE-OBS among the included frames (ISO text as stored);
    /// `DATE-END` is written only when this is Some.
    pub date_obs_last: Option<&'a str>,
    pub recipe: &'a str,        // IntegrationRecipe::describe()
    pub weight_mode: &'a str,   // the WeightMode serde name
    pub normalization: &'a str, // "<output>/<rejection>" serde names, e.g. "additiveWithScaling/scaleZeroOffset"
    pub reference_id: &'a str, // ATH_STKF — the reference frame's identity (Plan 5 decides the string)
    pub group_key: &'a str,    // ATH_STKG
    pub run_id: &'a str,       // ATH_STKI
    pub app_version: &'a str,
}

/// Build a master-light header: `IMAGETYP`/`SWCREATE`, the reference's
/// copy-through cards (minus `EXPTIME`/`DATE-OBS`, replaced below), the
/// group-level `NCOMBINE`/`EXPTIME`/`DATE-OBS`/`DATE-END`, the reference's
/// WCS (when it has one, unchanged — the master is in reference geometry),
/// and the `ATH_STK*` provenance cards.
pub fn build_master_light_cards(
    inputs: &MasterCardInputs<'_>,
) -> Result<Vec<Card>, FitsWriteError> {
    let mut cards = HeaderBuilder::new(FrameKind::MasterLight)
        .swcreate(inputs.app_version)
        .build()?;

    cards.extend(
        inputs
            .reference_cards
            .iter()
            .filter(|c| REGISTERED_COPY_THROUGH.contains(&c.keyword.as_str()))
            // EXPTIME is always replaced below (the weighted total is always
            // known); DATE-OBS is replaced only when the group has one of
            // its own — otherwise the reference's own card is kept.
            .filter(|c| c.keyword != "EXPTIME")
            .filter(|c| inputs.date_obs_first.is_none() || c.keyword != "DATE-OBS")
            .cloned(),
    );

    cards.push(
        Card::new("NCOMBINE", CardValue::Integer(inputs.frames as i64))?
            .with_comment("frames combined"),
    );
    cards.push(
        Card::new("EXPTIME", CardValue::Real(inputs.weighted_exposure_s))?
            .with_comment("weighted total exposure, s"),
    );
    if let Some(first) = inputs.date_obs_first {
        cards.push(Card::new("DATE-OBS", CardValue::Str(first.to_string()))?);
    }
    if let Some(last) = inputs.date_obs_last {
        cards.push(Card::new("DATE-END", CardValue::Str(last.to_string()))?);
    }

    if let Some(solve) = inputs.wcs {
        cards.extend(wcs_cards(solve)?);
    }

    cards.push(
        Card::new("ATH_STK", CardValue::Logical(true))?
            .with_comment("stacked by Athenaeum; never cataloged"),
    );
    cards.push(
        Card::new("ATH_STKV", CardValue::Integer(ATH_STK_VERSION))?
            .with_comment("stacking header version"),
    );
    cards.push(
        Card::new("ATH_STKN", CardValue::Integer(inputs.frames as i64))?
            .with_comment("frames combined"),
    );
    cards.push(
        Card::new("ATH_STKW", CardValue::Str(inputs.weight_mode.to_string()))?
            .with_comment("weight mode"),
    );
    // Variable-length values: no card comment — value + comment must fit one
    // 80-byte record and format_card errors otherwise (the same rule
    // register/writer.rs applies to ATH_REGT). ATH_STKG looks bounded
    // (`<instrume>__<mono|osc>__<filter>__bin<n>__<w>x<h>[__<exp>s]`) but the
    // sanitized INSTRUME/FILTER strings inside it are caller text, not fixed
    // width, so it belongs here too.
    cards.push(Card::new(
        "ATH_STKR",
        CardValue::Str(inputs.recipe.to_string()),
    )?);
    cards.push(Card::new(
        "ATH_STKO",
        CardValue::Str(inputs.normalization.to_string()),
    )?);
    cards.push(Card::new(
        "ATH_STKF",
        CardValue::Str(inputs.reference_id.to_string()),
    )?);
    cards.push(Card::new(
        "ATH_STKG",
        CardValue::Str(inputs.group_key.to_string()),
    )?);
    cards.push(
        Card::new("ATH_STKI", CardValue::Str(inputs.run_id.to_string()))?
            .with_comment("stacking run"),
    );

    Ok(cards)
}

/// Trim, then treat a blank result as absent — a `Some("  ")` filter/instrume
/// must fall back to the default just like `None` does.
fn blank(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

fn fmt_exposure_number(x: f64) -> String {
    if x == x.trunc() {
        format!("{x:.0}")
    } else {
        format!("{x:.1}")
    }
}

/// §9.5: `<set slug>_<filter>_<instrume>_<n>x<exp>s.fits` when every exposure
/// is within ±0.5 s of the first, else `<set slug>_<filter>_<instrume>_<n>f_<total>s.fits`;
/// every part sanitized; `exp`/`total` printed as integers when whole, else
/// one decimal. "Equal" means every exposure within ±0.5 s of the FIRST
/// one — order-dependent by design (the first frame anchors the name).
pub fn master_file_name(
    set_name: &str,
    filter: Option<&str>,
    instrume: Option<&str>,
    exposures_s: &[f64],
) -> String {
    let mut slug = sanitize_for_filename(set_name);
    if slug.is_empty() {
        slug = "set".to_string();
    }
    let filter = sanitize_for_filename(blank(filter).unwrap_or("NoFilter"));
    let instrume = sanitize_for_filename(blank(instrume).unwrap_or("unknown"));
    let n = exposures_s.len();
    let e0 = exposures_s.first().copied().unwrap_or(0.0);
    let equal = exposures_s.iter().all(|&e| (e - e0).abs() <= 0.5);
    let tail = if equal {
        format!("{n}x{}s", fmt_exposure_number(e0))
    } else {
        let total: f64 = exposures_s.iter().sum();
        format!("{n}f_{}s", fmt_exposure_number(total))
    };
    format!("{slug}_{filter}_{instrume}_{tail}.fits")
}

pub struct WrittenMaster {
    pub master: PathBuf,
    pub rejection_low: Option<PathBuf>,
    pub rejection_high: Option<PathBuf>,
}

/// Header cards for one rejection map: `IMAGETYP` `Rejection Map Low`/`High`
/// (a plain card, not `FrameKind` — these are Athenaeum-only artifacts, not
/// one of the frame kinds the catalog knows), `ATH_STK`/`ATH_STKV`, `BUNIT`,
/// and the master's own `ATH_STKI`/`ATH_STKG` cards copied through so a map
/// can be traced back to its run and group without opening the master too.
fn rejection_map_cards(label: &str, master_cards: &[Card]) -> Result<Vec<Card>, FitsWriteError> {
    let mut cards = vec![
        Card::new("IMAGETYP", CardValue::Str(format!("Rejection Map {label}")))?,
        Card::new("ATH_STK", CardValue::Logical(true))?
            .with_comment("stacked by Athenaeum; never cataloged"),
        Card::new("ATH_STKV", CardValue::Integer(ATH_STK_VERSION))?
            .with_comment("stacking header version"),
        Card::new("BUNIT", CardValue::Str("count".into()))?.with_comment("rejected-sample count"),
    ];
    for kw in ["ATH_STKI", "ATH_STKG"] {
        if let Some(c) = master_cards.iter().find(|c| c.keyword == kw) {
            cards.push(c.clone());
        }
    }
    Ok(cards)
}

/// Writes the master (planar `channels × w × h`) and, when present, the two
/// maps as `<stem>_rejlow.fits` / `<stem>_rejhigh.fits`; returns the paths
/// written. Never overwrites: `resolve_collision` on the master path, the
/// map names derived from the resolved stem (so a collision-suffixed master
/// still names its own maps, not a sibling's).
/// Not atomic as a set: the master lands before the maps, so a failed map
/// write leaves the master on disk with no map (re-running never overwrites
/// it — resolve_collision suffixes the retry). Two runs writing into one
/// folder concurrently can resolve the same free name (resolve_collision is
/// check-then-write); the orchestrator serializes group writes.
pub fn write_master_light(
    dir: &Path,
    file_name: &str,
    output: &GroupOutput,
    cards: &[Card],
) -> anyhow::Result<WrittenMaster> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;

    let master_path = resolve_collision(&dir.join(file_name));
    write_fits_f32(
        &master_path,
        output.width,
        output.height,
        output.channels,
        &output.data,
        cards,
    )
    .with_context(|| format!("writing {}", master_path.display()))?;

    let stem = master_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("master")
        .to_string();

    let write_map = |suffix: &str, label: &str, data: &[f32]| -> anyhow::Result<PathBuf> {
        let path = resolve_collision(&dir.join(format!("{stem}_{suffix}.fits")));
        let map_cards = rejection_map_cards(label, cards)?;
        write_fits_f32(
            &path,
            output.width,
            output.height,
            output.channels,
            data,
            &map_cards,
        )
        .with_context(|| format!("writing {}", path.display()))?;
        Ok(path)
    };

    let rejection_low = output
        .rejection_low
        .as_deref()
        .map(|data| write_map("rejlow", "Low", data))
        .transpose()?;
    let rejection_high = output
        .rejection_high
        .as_deref()
        .map(|data| write_map("rejhigh", "High", data))
        .transpose()?;

    Ok(WrittenMaster {
        master: master_path,
        rejection_low,
        rejection_high,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_parser::FitsHeader;
    use crate::stacking::integrate::GroupStats;

    fn dummy_stats() -> GroupStats {
        GroupStats {
            frames: 3,
            included: 3,
            dropped_below_min_weight: 0,
            recipe: "test".into(),
            rejected_low_fraction: 0.0,
            rejected_high_fraction: 0.0,
            rejected_fraction_per_frame: vec![0.0; 3],
            master_noise: vec![0.0; 3],
            master_location: vec![0.0; 3],
            master_scale: vec![0.0; 3],
            best_sub_noise: vec![0.0; 3],
            master_psf_snr: vec![0.0; 3],
            best_sub_psf_snr: vec![0.0; 3],
            snr_gain: vec![0.0; 3],
            master_fwhm_px: vec![0.0; 3],
            master_eccentricity: vec![0.0; 3],
            weighted_exposure_s: 0.0,
            total_exposure_s: 0.0,
            read_ms: 0,
            combine_ms: 0,
            bytes_read: 0,
        }
    }

    // Solver-shaped SIP tables (see fits_writer::wcs's own tests): square
    // (order+1)x(order+1), non-zero low-order terms.
    const SIP_A: &str = "[[1.0e-6,2.0e-6,1.0e-7],[3.0e-6,2.0e-7,0.0],[3.0e-7,0.0,0.0]]";
    const SIP_B: &str = "[[-1.0e-6,-2.0e-6,-1.0e-7],[-3.0e-6,-2.0e-7,0.0],[-3.0e-7,0.0,0.0]]";

    fn record(sip: bool) -> PlateSolveRecord {
        PlateSolveRecord {
            id: None,
            frame_id: 1,
            crpix1: 3111.5,
            crpix2: 2083.5,
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

    #[test]
    fn master_name_follows_the_layout_rules() {
        assert_eq!(
            master_file_name(
                "LDN 1272",
                Some("NoFilter"),
                Some("atr2600m"),
                &[180.0; 208]
            ),
            "LDN_1272_NoFilter_atr2600m_208x180s.fits"
        );
        assert_eq!(
            master_file_name(
                "M 31",
                Some("Ha"),
                Some("ASI 2600MM"),
                &[300.0, 300.4, 299.6]
            ),
            "M_31_Ha_ASI_2600MM_3x300s.fits"
        );
        assert_eq!(
            master_file_name("M 31", None, None, &[120.0, 180.0, 300.0]),
            "M_31_NoFilter_unknown_3f_600s.fits"
        );
        assert_eq!(
            master_file_name("a/b:c", Some("L"), Some("cam"), &[0.5, 0.5]),
            "a_b_c_L_cam_2x0.5s.fits"
        );
        assert_eq!(
            master_file_name("M 31", Some("  "), Some(""), &[60.0, 60.0]),
            "M_31_NoFilter_unknown_2x60s.fits"
        );
        assert_eq!(
            master_file_name("...", Some("L"), Some("cam"), &[60.0]),
            "set_L_cam_1x60s.fits"
        );
        assert_eq!(
            master_file_name("M 31", Some("Ha"), Some("cam"), &[300.0, 300.4, 300.4]),
            "M_31_Ha_cam_3x300s.fits"
        );
    }

    #[test]
    fn master_cards_carry_copy_through_wcs_and_provenance() {
        let reference_cards = vec![
            Card::new("EXPTIME", CardValue::Real(180.0)).unwrap(),
            Card::new("INSTRUME", CardValue::Str("cam".into())).unwrap(),
            Card::new("OBJECT", CardValue::Str("LDN 1272".into())).unwrap(),
            Card::new("ROWORDER", CardValue::Str("TOP-DOWN".into())).unwrap(),
            Card::new("DATE-OBS", CardValue::Str("2025-10-18T02:02:02".into())).unwrap(),
            Card::new("BAYERPAT", CardValue::Str("RGGB".into())).unwrap(), // must not survive
        ];
        let solve = record(true);
        let cards = build_master_light_cards(&MasterCardInputs {
            reference_cards: &reference_cards,
            wcs: Some(&solve),
            frames: 208,
            weighted_exposure_s: 36000.0,
            date_obs_first: Some("2025-09-14T00:56:00"),
            date_obs_last: Some("2025-10-19T05:25:52"),
            recipe: "Average | Linear fit clip (5.0/3.5)",
            weight_mode: "psfSignalWeight",
            normalization: "additiveWithScaling/scaleZeroOffset",
            reference_id: "frame:73",
            group_key: "atr2600m__mono__NoFilter__bin1__6224x4168",
            run_id: "run-7",
            app_version: "0.5.7",
        })
        .unwrap();
        let kw = |k: &str| {
            cards
                .iter()
                .find(|c| c.keyword == k)
                .map(|c| c.value.clone().unwrap())
        };
        assert_eq!(kw("IMAGETYP"), Some(CardValue::Str("Master Light".into())));
        assert_eq!(kw("NCOMBINE"), Some(CardValue::Integer(208)));
        assert_eq!(kw("EXPTIME"), Some(CardValue::Real(36000.0)));
        assert_eq!(
            kw("DATE-OBS"),
            Some(CardValue::Str("2025-09-14T00:56:00".into()))
        );
        assert_eq!(
            kw("DATE-END"),
            Some(CardValue::Str("2025-10-19T05:25:52".into()))
        );
        assert_eq!(kw("ROWORDER"), Some(CardValue::Str("TOP-DOWN".into())));
        assert_eq!(kw("OBJECT"), Some(CardValue::Str("LDN 1272".into())));
        assert_eq!(kw("INSTRUME"), Some(CardValue::Str("cam".into())));
        assert_eq!(kw("BAYERPAT"), None);
        assert_eq!(kw("CTYPE1"), Some(CardValue::Str("RA---TAN-SIP".into())));
        assert_eq!(kw("ATH_STK"), Some(CardValue::Logical(true)));
        assert_eq!(kw("ATH_STKV"), Some(CardValue::Integer(1)));
        assert_eq!(kw("ATH_STKN"), Some(CardValue::Integer(208)));
        assert_eq!(
            kw("ATH_STKR"),
            Some(CardValue::Str("Average | Linear fit clip (5.0/3.5)".into()))
        );
        assert_eq!(
            kw("ATH_STKW"),
            Some(CardValue::Str("psfSignalWeight".into()))
        );
        assert_eq!(
            kw("ATH_STKO"),
            Some(CardValue::Str("additiveWithScaling/scaleZeroOffset".into()))
        );
        assert_eq!(kw("ATH_STKF"), Some(CardValue::Str("frame:73".into())));
        assert_eq!(
            kw("ATH_STKG"),
            Some(CardValue::Str(
                "atr2600m__mono__NoFilter__bin1__6224x4168".into()
            ))
        );
        assert_eq!(kw("ATH_STKI"), Some(CardValue::Str("run-7".into())));
        assert!(kw("SWCREATE").is_some());
        // exactly one of each — the reference's EXPTIME/DATE-OBS were replaced, not duplicated
        assert_eq!(cards.iter().filter(|c| c.keyword == "EXPTIME").count(), 1);
        assert_eq!(cards.iter().filter(|c| c.keyword == "DATE-OBS").count(), 1);

        // Every card must format into 80-byte records — the provenance
        // values are caller strings and a comment would overflow them.
        for c in &cards {
            crate::fits_writer::card::format_card(c)
                .unwrap_or_else(|e| panic!("{}: {e}", c.keyword));
        }

        // A long, real-world reference id (150 chars: a full source path)
        // and a normalization value that alone would overflow ATH_STKO's
        // old comment (`multiplicativeWithScaling/scaleZeroOffset`, 41
        // chars) must still format — ATH_STKF goes through the CONTINUE
        // chain and reads back whole.
        let long_reference_id = "file:/Volumes/BigMac/Users/astrobureau/Pictures/Calibration Test/LDN1272-WBPP/LDN1272-ATH/LDN 1272/camera_atr2600m/lights/c_2025-10-18_02-02-02__-9.90_180.00s_0073.fits";
        let long_cards = build_master_light_cards(&MasterCardInputs {
            reference_cards: &reference_cards,
            wcs: Some(&solve),
            frames: 208,
            weighted_exposure_s: 36000.0,
            date_obs_first: Some("2025-09-14T00:56:00"),
            date_obs_last: Some("2025-10-19T05:25:52"),
            recipe: "Average | Linear fit clip (5.0/3.5)",
            weight_mode: "psfSignalWeight",
            normalization: "multiplicativeWithScaling/scaleZeroOffset",
            reference_id: long_reference_id,
            group_key: "zwoasi2600mcduo__osc__NoFilter__bin1__6248x4176__180s",
            run_id: "run-7",
            app_version: "0.5.7",
        })
        .unwrap();
        for c in &long_cards {
            crate::fits_writer::card::format_card(c)
                .unwrap_or_else(|e| panic!("{}: {e}", c.keyword));
        }
        let long_kw = |k: &str| {
            long_cards
                .iter()
                .find(|c| c.keyword == k)
                .map(|c| c.value.clone().unwrap())
        };
        assert_eq!(
            long_kw("ATH_STKF"),
            Some(CardValue::Str(long_reference_id.to_string()))
        );
    }

    #[test]
    fn date_obs_falls_back_to_the_reference_card_when_the_group_has_none() {
        let reference_cards = vec![
            Card::new("EXPTIME", CardValue::Real(180.0)).unwrap(),
            Card::new("DATE-OBS", CardValue::Str("2025-10-18T02:02:02".into())).unwrap(),
        ];
        let cards = build_master_light_cards(&MasterCardInputs {
            reference_cards: &reference_cards,
            wcs: None,
            frames: 5,
            weighted_exposure_s: 900.0,
            date_obs_first: None,
            date_obs_last: None,
            recipe: "Average | Percentile clip (0.2/0.1)",
            weight_mode: "psfSignalWeight",
            normalization: "additiveWithScaling/scaleZeroOffset",
            reference_id: "frame:1",
            group_key: "cam__mono__NoFilter__bin1__100x100",
            run_id: "run-1",
            app_version: "0.5.7",
        })
        .unwrap();
        let kw = |k: &str| {
            cards
                .iter()
                .find(|c| c.keyword == k)
                .map(|c| c.value.clone().unwrap())
        };
        assert_eq!(
            kw("DATE-OBS"),
            Some(CardValue::Str("2025-10-18T02:02:02".into()))
        );
        assert_eq!(kw("DATE-END"), None);
        assert_eq!(cards.iter().filter(|c| c.keyword == "DATE-OBS").count(), 1);
    }

    #[test]
    fn master_cards_without_a_solve_carry_no_wcs() {
        let reference_cards = vec![
            Card::new("EXPTIME", CardValue::Real(180.0)).unwrap(),
            Card::new("INSTRUME", CardValue::Str("cam".into())).unwrap(),
            Card::new("OBJECT", CardValue::Str("LDN 1272".into())).unwrap(),
            Card::new("ROWORDER", CardValue::Str("TOP-DOWN".into())).unwrap(),
            Card::new("DATE-OBS", CardValue::Str("2025-10-18T02:02:02".into())).unwrap(),
            Card::new("BAYERPAT", CardValue::Str("RGGB".into())).unwrap(),
        ];
        let cards = build_master_light_cards(&MasterCardInputs {
            reference_cards: &reference_cards,
            wcs: None,
            frames: 208,
            weighted_exposure_s: 36000.0,
            date_obs_first: Some("2025-09-14T00:56:00"),
            date_obs_last: Some("2025-10-19T05:25:52"),
            recipe: "Average | Linear fit clip (5.0/3.5)",
            weight_mode: "psfSignalWeight",
            normalization: "additiveWithScaling/scaleZeroOffset",
            reference_id: "frame:73",
            group_key: "atr2600m__mono__NoFilter__bin1__6224x4168",
            run_id: "run-7",
            app_version: "0.5.7",
        })
        .unwrap();
        let forbidden_prefixes = ["CTYPE", "CRPIX", "CRVAL", "CD", "A_", "PLTSOLVD"];
        for c in &cards {
            assert!(
                !forbidden_prefixes.iter().any(|p| c.keyword.starts_with(p)),
                "unexpected WCS card {}",
                c.keyword
            );
        }
        assert_eq!(
            cards
                .iter()
                .find(|c| c.keyword == "ATH_STK")
                .map(|c| c.value.clone().unwrap()),
            Some(CardValue::Logical(true))
        );
    }

    #[test]
    fn writer_lands_master_and_maps_without_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h, ch) = (8usize, 6usize, 3usize);
        let plane = w * h;

        let mut cards = HeaderBuilder::new(FrameKind::MasterLight).build().unwrap();
        cards.push(Card::new("ATH_STKI", CardValue::Str("run-1".into())).unwrap());
        cards.push(Card::new("ATH_STKG", CardValue::Str("group-1".into())).unwrap());

        let output = GroupOutput {
            width: w,
            height: h,
            channels: ch,
            data: vec![0.5f32; plane * ch],
            rejection_low: Some(vec![1.0f32; plane * ch]),
            rejection_high: Some(vec![2.0f32; plane * ch]),
            included: vec![0, 1, 2],
            stats: dummy_stats(),
        };

        let first = write_master_light(dir.path(), "master.fits", &output, &cards).unwrap();
        assert_eq!(
            first.master.file_name().and_then(|s| s.to_str()),
            Some("master.fits")
        );
        let rejlow = first.rejection_low.clone().unwrap();
        let rejhigh = first.rejection_high.clone().unwrap();
        assert_eq!(
            rejlow.file_name().and_then(|s| s.to_str()),
            Some("master_rejlow.fits")
        );
        assert_eq!(
            rejhigh.file_name().and_then(|s| s.to_str()),
            Some("master_rejhigh.fits")
        );

        let header = FitsHeader::from_path(&first.master).unwrap();
        assert_eq!(header.get_str("IMAGETYP").as_deref(), Some("Master Light"));
        assert_eq!(header.get_i32("NAXIS3"), Some(3));

        let low_header = FitsHeader::from_path(&rejlow).unwrap();
        assert_eq!(
            low_header.get_str("IMAGETYP").as_deref(),
            Some("Rejection Map Low")
        );
        assert_eq!(low_header.get_str("ATH_STK").as_deref(), Some("T"));
        assert_eq!(low_header.get_str("ATH_STKI").as_deref(), Some("run-1"));
        assert_eq!(low_header.get_str("ATH_STKG").as_deref(), Some("group-1"));

        let high_header = FitsHeader::from_path(&rejhigh).unwrap();
        assert_eq!(
            high_header.get_str("IMAGETYP").as_deref(),
            Some("Rejection Map High")
        );
        assert_eq!(high_header.get_str("ATH_STK").as_deref(), Some("T"));

        // Write again with the same file name → collision-suffixed names, no overwrite.
        let second = write_master_light(dir.path(), "master.fits", &output, &cards).unwrap();
        assert_eq!(
            second.master.file_name().and_then(|s| s.to_str()),
            Some("master_2.fits")
        );
        assert_eq!(
            second
                .rejection_low
                .unwrap()
                .file_name()
                .and_then(|s| s.to_str()),
            Some("master_2_rejlow.fits")
        );
        assert_eq!(
            second
                .rejection_high
                .unwrap()
                .file_name()
                .and_then(|s| s.to_str()),
            Some("master_2_rejhigh.fits")
        );
        assert!(first.master.exists());
        assert!(second.master.exists());
    }
}

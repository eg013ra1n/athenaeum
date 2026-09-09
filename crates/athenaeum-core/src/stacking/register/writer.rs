//! Optional registered frames (spec §3.7): the subject resampled once into
//! the reference geometry, NaN outside its coverage, with the source's
//! acquisition cards and the `ATH_REG*` provenance. Artifacts, never
//! cataloged; nothing downstream reads them.

use std::path::Path;

use anyhow::Context;

use crate::calibration_library::light_resolve::card_from_kv;
use crate::fits_parser::stored_header::parse_stored_header_keys;
use crate::fits_parser::FitsHeader;
use crate::fits_writer::{write_fits_f32, Card, CardValue};
use crate::geometry::PixelMap;
use crate::integration::plane_reader::PlaneReader;
use crate::models::FileFormat;
use crate::resample::{warp_rows, Interpolation, Plane};

/// Source cards a registered frame keeps: acquisition and target metadata
/// only. The pixel grid is the reference's, so the subject's WCS is wrong
/// for it, and the frame is resampled, so it has no CFA.
/// `ROWORDER` stays: it is orientation, not CFA. The registered array is the
/// reference's grid, so within one camera group the subject's card is the
/// reference's too; for a cross-camera group the caller stamps the
/// reference's value through `RegisteredCards::reference_roworder`.
pub const REGISTERED_COPY_THROUGH: &[&str] = &[
    "EXPTIME", "GAIN", "OFFSET", "EGAIN", "XBINNING", "YBINNING", "XPIXSZ", "YPIXSZ", "CCD-TEMP",
    "SET-TEMP", "INSTRUME", "TELESCOP", "FOCALLEN", "APTDIA", "FILTER", "DATE-OBS", "OBJECT",
    "OBJCTRA", "OBJCTDEC", "RA", "DEC", "SITELAT", "SITELONG", "SITEELEV", "OBSERVER", "ROWORDER",
];

pub const ATH_REG_VERSION: i64 = 1;

pub struct RegisteredCards<'a> {
    pub reference_name: &'a str,
    pub model: &'a str,
    pub transform_json: &'a str,
    pub interpolation: Interpolation,
    pub clamping: f32,
    pub rms_px: f64,
    /// `ROWORDER` of the reference frame; when set it replaces the subject's
    /// card (the registered array is the reference's grid).
    pub reference_roworder: Option<&'a str>,
}

/// The copy-through cards of a FITS file, read from its own header (no
/// catalog needed).
pub fn source_cards_from_file(path: &Path) -> anyhow::Result<Vec<Card>> {
    let header = FitsHeader::from_path(path)
        .with_context(|| format!("reading header of {}", path.display()))?;
    let keys = parse_stored_header_keys(FileFormat::FITS, &header.to_header_text());
    let mut cards: Vec<Card> = keys
        .iter()
        .filter(|(k, _)| REGISTERED_COPY_THROUGH.contains(&k.to_ascii_uppercase().as_str()))
        .filter_map(|(k, v)| card_from_kv(k, v))
        .collect();
    cards.sort_by(|a, b| a.keyword.cmp(&b.keyword));
    Ok(cards)
}

/// Copy-through cards filtered once more, then `IMAGETYP` and the
/// `ATH_REG*` provenance.
pub fn build_registered_cards(source: &[Card], reg: &RegisteredCards) -> anyhow::Result<Vec<Card>> {
    let mut cards: Vec<Card> = source
        .iter()
        .filter(|c| REGISTERED_COPY_THROUGH.contains(&c.keyword.as_str()))
        .cloned()
        .collect();
    if let Some(order) = reg.reference_roworder {
        cards.retain(|c| c.keyword != "ROWORDER");
        cards.push(
            Card::new("ROWORDER", CardValue::Str(order.into()))?
                .with_comment("row order of the reference grid"),
        );
    }
    let kernel = serde_json::to_value(reg.interpolation)?
        .as_str()
        .unwrap_or("")
        .to_string();
    cards.push(
        Card::new("IMAGETYP", CardValue::Str("LIGHT".into()))?.with_comment("registered light"),
    );
    cards.push(
        Card::new("ATH_REG", CardValue::Logical(true))?
            .with_comment("registered by Athenaeum; never cataloged"),
    );
    cards.push(
        Card::new("ATH_REGV", CardValue::Integer(ATH_REG_VERSION))?
            .with_comment("registered-frame format version"),
    );
    cards.push(
        Card::new("ATH_REGR", CardValue::Str(reg.reference_name.to_string()))?
            .with_comment("reference frame"),
    );
    cards.push(
        Card::new("ATH_REGM", CardValue::Str(reg.model.to_string()))?
            .with_comment("registration model"),
    );
    cards.push(Card::new("ATH_REGK", CardValue::Str(kernel))?.with_comment("resampling kernel"));
    cards.push(
        Card::new("ATH_REGC", CardValue::Real(reg.clamping as f64))?
            .with_comment("kernel clamping threshold"),
    );
    cards.push(
        Card::new("ATH_REGS", CardValue::Real(reg.rms_px))?.with_comment("[px] registration RMS"),
    );
    // Arbitrary length → CONTINUE chain; no comment (it could overflow the last record).
    cards.push(Card::new(
        "ATH_REGT",
        CardValue::Str(reg.transform_json.to_string()),
    )?);
    Ok(cards)
}

/// Resample every plane of `subject` into the reference geometry through
/// `map` (subject → reference; the resampler gathers through its inverse)
/// and write the float32 FITS. Returns the plane count.
pub fn write_registered_frame(
    subject: &Path,
    map: &PixelMap,
    ref_w: usize,
    ref_h: usize,
    interp: Interpolation,
    clamping: f32,
    cards: &[Card],
    out: &Path,
) -> anyhow::Result<usize> {
    let reader = PlaneReader::open(subject)?;
    let (w, h, channels) = (reader.width(), reader.height(), reader.channels());
    let plane_len = ref_w * ref_h;
    let mut all = vec![f32::NAN; plane_len * channels];
    for plane in 0..channels {
        let data = reader.read_plane(plane)?;
        let src = Plane::full(&data, w, h);
        warp_rows(
            &src,
            map,
            ref_w,
            0,
            ref_h,
            interp,
            clamping,
            &mut all[plane * plane_len..(plane + 1) * plane_len],
        );
    }
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    write_fits_f32(out, ref_w, ref_h, channels, &all, cards)
        .with_context(|| format!("writing {}", out.display()))?;
    Ok(channels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_parser::FitsHeader;
    use crate::fits_writer::CardValue;
    use crate::geometry::{Linear, LinearKind};
    use crate::integration::plane_reader::PlaneReader;
    use crate::test_support::{centroid, gaussian_field};

    #[test]
    fn writes_the_subject_into_reference_geometry_with_provenance_cards() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (120usize, 90usize);
        // One star at (40.25, 30.5) in the subject; the map shifts by (+10, −5).
        let data = gaussian_field(w, h, &[(40.25, 30.5, 0.5)], 1.5, 0.05);
        let subject = dir.path().join("c_sub.fits");
        let src_cards = vec![
            Card::new("EXPTIME", CardValue::Real(180.0)).unwrap(),
            Card::new("INSTRUME", CardValue::Str("cam".into())).unwrap(),
            Card::new("CRVAL1", CardValue::Real(12.5)).unwrap(),
            Card::new("BAYERPAT", CardValue::Str("RGGB".into())).unwrap(),
            Card::new("DATE-OBS", CardValue::Str("2026-09-09T00:00:00".into())).unwrap(),
            Card::new("ROWORDER", CardValue::Str("TOP-DOWN".into())).unwrap(),
        ];
        write_fits_f32(&subject, w, h, 1, &data, &src_cards).unwrap();
        let map = PixelMap::linear(Linear::from_flat(
            LinearKind::Similarity,
            [1.0, 0.0, 10.0, 0.0, 1.0, -5.0, 0.0, 0.0, 1.0],
        ))
        .unwrap();
        let source = source_cards_from_file(&subject).unwrap();
        let kws: Vec<&str> = source.iter().map(|c| c.keyword.as_str()).collect();
        assert!(
            kws.contains(&"EXPTIME")
                && kws.contains(&"INSTRUME")
                && kws.contains(&"DATE-OBS")
                && kws.contains(&"ROWORDER")
        );
        assert!(
            !kws.contains(&"CRVAL1") && !kws.contains(&"BAYERPAT"),
            "{kws:?}"
        );
        let reg = RegisteredCards {
            reference_name: "c_ref",
            model: "homography",
            transform_json: &map.to_json(),
            interpolation: Interpolation::BicubicBSpline,
            clamping: 0.3,
            rms_px: 0.42,
            reference_roworder: None,
        };
        let cards = build_registered_cards(&source, &reg).unwrap();
        let out = dir.path().join("registered").join("r_sub.fits");
        let channels = write_registered_frame(
            &subject,
            &map,
            160,
            100,
            Interpolation::BicubicBSpline,
            0.3,
            &cards,
            &out,
        )
        .unwrap();
        assert_eq!(channels, 1);
        let r = PlaneReader::open(&out).unwrap();
        assert_eq!((r.width(), r.height(), r.channels()), (160, 100, 1));
        let plane = r.read_plane(0).unwrap();
        let (cx, cy) = centroid(&plane, 160, 50.25, 25.5, 5, 0.05);
        assert!(
            (cx - 50.25).abs() < 0.05 && (cy - 25.5).abs() < 0.05,
            "centroid {cx} {cy}"
        );
        assert!(
            plane[0].is_nan(),
            "outside the subject's coverage must be NaN"
        );
        assert!(plane[99 * 160 + 159].is_nan());
        let header = FitsHeader::from_path(&out).unwrap();
        assert_eq!(header.get_str("IMAGETYP").as_deref(), Some("LIGHT"));
        assert_eq!(header.get_str("ATH_REG").as_deref(), Some("T"));
        assert_eq!(header.get_i32("ATH_REGV"), Some(1));
        assert_eq!(header.get_str("ATH_REGR").as_deref(), Some("c_ref"));
        assert_eq!(header.get_str("ATH_REGM").as_deref(), Some("homography"));
        assert_eq!(
            header.get_str("ATH_REGK").as_deref(),
            Some("bicubicBSpline")
        );
        assert!((header.get_f64("ATH_REGC").unwrap() - 0.3).abs() < 1e-6);
        assert!((header.get_f64("ATH_REGS").unwrap() - 0.42).abs() < 1e-9);
        let back = PixelMap::from_json(&header.get_str("ATH_REGT").unwrap()).unwrap();
        assert_eq!(back.forward(1.0, 2.0), (11.0, -3.0));
        assert_eq!(header.get_f64("EXPTIME"), Some(180.0));
        assert_eq!(header.get_str("ROWORDER").as_deref(), Some("TOP-DOWN"));
        assert!(header.get_str("CRVAL1").is_none() && header.get_str("BAYERPAT").is_none());
    }

    #[test]
    fn reference_roworder_replaces_the_subject_card() {
        let source = vec![
            Card::new("EXPTIME", CardValue::Real(180.0)).unwrap(),
            Card::new("ROWORDER", CardValue::Str("TOP-DOWN".into())).unwrap(),
        ];
        let json = PixelMap::linear(Linear::identity()).unwrap().to_json();
        let reg = RegisteredCards {
            reference_name: "c_ref",
            model: "similarity",
            transform_json: &json,
            interpolation: Interpolation::BicubicBSpline,
            clamping: 0.3,
            rms_px: 0.42,
            reference_roworder: Some("BOTTOM-UP"),
        };
        let cards = build_registered_cards(&source, &reg).unwrap();
        let orders: Vec<&Card> = cards.iter().filter(|c| c.keyword == "ROWORDER").collect();
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].value, Some(CardValue::Str("BOTTOM-UP".into())));
        assert!(cards.iter().any(|c| c.keyword == "EXPTIME"));
    }

    #[test]
    fn three_plane_subjects_keep_their_planes() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (64usize, 48usize);
        let base = gaussian_field(w, h, &[(20.0, 20.0, 0.4)], 1.5, 0.05);
        let mut all = Vec::new();
        for k in 0..3 {
            all.extend(base.iter().map(|v| v * (1.0 - 0.25 * k as f32)));
        }
        let subject = dir.path().join("c_rgb.fits");
        write_fits_f32(&subject, w, h, 3, &all, &[]).unwrap();
        let map = PixelMap::linear(Linear::identity()).unwrap();
        let out = dir.path().join("r_rgb.fits");
        assert_eq!(
            write_registered_frame(
                &subject,
                &map,
                w,
                h,
                Interpolation::Bilinear,
                0.3,
                &[],
                &out
            )
            .unwrap(),
            3
        );
        let r = PlaneReader::open(&out).unwrap();
        let p0 = r.read_plane(0).unwrap();
        let p2 = r.read_plane(2).unwrap();
        assert!((p0[20 * w + 20] - base[20 * w + 20]).abs() < 1e-5);
        assert!((p2[20 * w + 20] - 0.5 * base[20 * w + 20]).abs() < 1e-5);
    }
}

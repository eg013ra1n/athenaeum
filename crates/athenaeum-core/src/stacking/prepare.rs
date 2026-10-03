//! Prepare — stage 1 of a run whose frames are already calibrated (spec
//! 2026-10-02 §5.1, §13; plan 6b-2 C12): a project's landed replicas and an
//! attested frame set's lights. A file the plane reader decodes as unit-
//! domain float32/64 is used IN PLACE; an integer FITS or an XISF is
//! converted to float32 FITS; a CFA mosaic is debayered when `debayerOsc`
//! is on; a file no reader here can decode is excluded "unsupported file".
//! This part classifies a file from its header alone (the plan's space
//! estimate and warnings, and the run, read the same answer).

use std::path::Path;

use crate::integration::cfa::CfaGeometry;
use crate::stacking::origin::cfa_geometry_of_file;
use crate::stacking::project_source::{is_xisf, xisf_geometry};

#[derive(Debug, Clone, PartialEq)]
pub enum FileKind {
    /// A primary-HDU FITS image, BITPIX −32 / −64.
    FloatFits,
    /// A primary-HDU FITS image, BITPIX 8 / 16 / 32 (BZERO / BSCALE applied
    /// by the reader: physical ADU, not unit domain).
    IntFits { bitpix: i32 },
    /// A monolithic XISF (attachment or embedded data).
    Xisf,
    /// A file Prepare cannot read, with why.
    Unsupported(String),
}

/// What a file is, from its header.
#[derive(Debug, Clone, PartialEq)]
pub struct FileShape {
    pub kind: FileKind,
    pub width: usize,
    pub height: usize,
    pub planes: usize,
    /// The file's own CFA declaration — only ever for a one-plane image.
    pub cfa: Option<CfaGeometry>,
}

/// How Prepare reads a file it writes a copy of.
#[derive(Debug, Clone, PartialEq)]
pub enum PrepareRead {
    /// A float FITS read as it is (only ever to debayer it).
    InPlaceFits,
    /// An integer FITS: physical values ÷ `divisor` (the calibrated-light
    /// generator's own `scale_divisor_for_bitpix`), into the unit domain.
    ScaledFits { divisor: f64 },
    /// An XISF through `read_raw`, ÷ 65535 (its documented scale).
    Xisf,
}

/// What Prepare does with one file (plan 6b-2 C12).
#[derive(Debug, Clone, PartialEq)]
pub enum PrepareAction {
    /// Used in place: the `calibrated` artifact points at the file itself.
    InPlace,
    /// A float32 FITS copy is written (converted when `read` is not
    /// `InPlaceFits`), then debayered when `debayer` is set.
    Write {
        read: PrepareRead,
        debayer: Option<CfaGeometry>,
    },
    /// The frame is excluded "unsupported file: <why>".
    Unsupported(String),
}

/// Classify a file from its header alone; never fails — an unreadable file
/// is `Unsupported` with the reason.
pub fn probe_file(path: &Path) -> FileShape {
    let unsupported = |reason: String| FileShape {
        kind: FileKind::Unsupported(reason),
        width: 0,
        height: 0,
        planes: 0,
        cfa: None,
    };
    let cfa_of = |p: &Path| match cfa_geometry_of_file(p) {
        Ok(geometry) => geometry,
        Err(e) => {
            tracing::warn!(path = %p.display(), error = %format!("{e:#}"), "reading the file's cfa header failed; treated as no mosaic");
            None
        }
    };
    // Review I10: a file that cannot be opened (offline, renamed, archived)
    // says so, instead of looking like an image no reader here decodes.
    if let Err(e) = std::fs::File::open(path) {
        return unsupported(format!("cannot open the file: {e}"));
    }
    if is_xisf(path) {
        let xml = match crate::fits_parser::read_xisf_header(path) {
            Ok(raw) => String::from_utf8_lossy(&raw).into_owned(),
            Err(e) => return unsupported(format!("unreadable XISF header: {e:#}")),
        };
        if xml.contains("location=\"path(") || xml.contains("location=\"url(") {
            return unsupported("distributed XISF".to_string());
        }
        let Some((w, h, c)) = xisf_geometry(&xml) else {
            return unsupported("XISF without an image geometry".to_string());
        };
        if c != 1 && c != 3 {
            return unsupported(format!("XISF with {c} channels"));
        }
        return FileShape {
            kind: FileKind::Xisf,
            width: w,
            height: h,
            planes: c,
            cfa: if c == 1 { cfa_of(path) } else { None },
        };
    }
    match crate::integration::banded::probe_fits(path) {
        Some((_file, info)) => FileShape {
            kind: if matches!(info.bitpix, -32 | -64) {
                FileKind::FloatFits
            } else {
                FileKind::IntFits {
                    bitpix: info.bitpix,
                }
            },
            width: info.w,
            height: info.h,
            planes: info.naxis3,
            cfa: if info.naxis3 == 1 { cfa_of(path) } else { None },
        },
        // Also a tile-compressed FITS or an extension image (primary NAXIS 0),
        // BITPIX 64, or a 4-plane cube.
        None => unsupported("not a 1- or 3-plane 8/16/32-bit or float primary image".to_string()),
    }
}

/// The rule of plan 6b-2 C12: in place only for a float FITS that needs no
/// debayer; a one-plane mosaic is debayered when `debayer_osc` is on; an
/// integer FITS or an XISF is converted.
pub fn prepare_action(shape: &FileShape, debayer_osc: bool) -> PrepareAction {
    let debayer = if debayer_osc && shape.planes == 1 {
        shape.cfa
    } else {
        None
    };
    match &shape.kind {
        FileKind::Unsupported(reason) => PrepareAction::Unsupported(reason.clone()),
        FileKind::FloatFits => match debayer {
            None => PrepareAction::InPlace,
            Some(geometry) => PrepareAction::Write {
                read: PrepareRead::InPlaceFits,
                debayer: Some(geometry),
            },
        },
        FileKind::IntFits { bitpix } => PrepareAction::Write {
            read: PrepareRead::ScaledFits {
                divisor: crate::calibration_library::light_cal::scale_divisor_for_bitpix(Some(
                    *bitpix,
                )),
            },
            debayer,
        },
        FileKind::Xisf => PrepareAction::Write {
            read: PrepareRead::Xisf,
            debayer,
        },
    }
}

/// The bytes Prepare writes for one file: the converted copy (its own planes)
/// unless the file is read in place, plus three planes when debayered.
pub fn prepared_bytes(shape: &FileShape, action: &PrepareAction) -> u64 {
    match action {
        PrepareAction::InPlace | PrepareAction::Unsupported(_) => 0,
        PrepareAction::Write { read, debayer } => {
            let plane = (shape.width * shape.height) as u64 * 4;
            let converted = if *read == PrepareRead::InPlaceFits {
                0
            } else {
                shape.planes as u64 * plane
            };
            let debayered = if debayer.is_some() { 3 * plane } else { 0 };
            converted + debayered
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::keywords::Bayer;
    use crate::fits_writer::{write_fits_f32, write_xisf_f32, Card, CardValue};

    fn mosaic_cards() -> Vec<Card> {
        vec![
            Card::new("BAYERPAT", CardValue::Str("RGGB".into())).unwrap(),
            Card::new("XBAYROFF", CardValue::Integer(0)).unwrap(),
            Card::new("YBAYROFF", CardValue::Integer(0)).unwrap(),
        ]
    }

    use crate::stacking::test_fixtures::write_empty_primary_fits as write_empty_primary;

    fn write_distributed_xisf(path: &Path) {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?><xisf version="1.0"><Image geometry="8:8:1" sampleFormat="Float32" location="path(@header_dir/data.bin)"/></xisf>"#;
        let mut bytes = b"XISF0100".to_vec();
        bytes.extend_from_slice(&(xml.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&[0u8; 4]);
        bytes.extend_from_slice(xml.as_bytes());
        std::fs::write(path, bytes).unwrap();
    }

    /// Plan 6b-2 C12: what each kind of file becomes — in place only for a
    /// float FITS the plane reader decodes that needs no debayer.
    #[test]
    fn the_prepare_action_of_each_file_kind() {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n);
        let rggb = CfaGeometry {
            pattern: Bayer::Rggb,
            xoff: 0,
            yoff: 0,
        };
        write_fits_f32(&p("mono.fits"), 8, 8, 1, &[0.1; 64], &[]).unwrap();
        write_fits_f32(&p("rgb.fits"), 8, 8, 3, &[0.1; 192], &[]).unwrap();
        write_fits_f32(&p("mosaic.fits"), 8, 8, 1, &[0.1; 64], &mosaic_cards()).unwrap();
        crate::stacking::test_fixtures::write_fits_i16(&p("adu.fits"), 8, 8, &[100i16; 64], &[]);
        write_xisf_f32(&p("x.xisf"), 8, 8, 1, &[0.1; 64], &[]).unwrap();
        write_empty_primary(&p("tile.fits"));
        write_distributed_xisf(&p("dist.xisf"));

        let action = |n: &str, debayer| prepare_action(&probe_file(&p(n)), debayer);
        assert_eq!(action("mono.fits", true), PrepareAction::InPlace);
        assert_eq!(action("rgb.fits", true), PrepareAction::InPlace);
        assert_eq!(
            action("mosaic.fits", true),
            PrepareAction::Write {
                read: PrepareRead::InPlaceFits,
                debayer: Some(rggb)
            }
        );
        assert_eq!(action("mosaic.fits", false), PrepareAction::InPlace);
        assert_eq!(
            action("adu.fits", true),
            PrepareAction::Write {
                read: PrepareRead::ScaledFits { divisor: 65535.0 },
                debayer: None
            }
        );
        assert_eq!(
            action("x.xisf", true),
            PrepareAction::Write {
                read: PrepareRead::Xisf,
                debayer: None
            }
        );
        assert_eq!(
            action("tile.fits", true),
            PrepareAction::Unsupported(
                "not a 1- or 3-plane 8/16/32-bit or float primary image".into()
            )
        );
        assert_eq!(
            action("dist.xisf", true),
            PrepareAction::Unsupported("distributed XISF".into())
        );
        // Review I10: a file that cannot be opened says so.
        assert!(
            matches!(action("absent.fits", true), PrepareAction::Unsupported(ref m) if m.starts_with("cannot open the file: ")),
            "{:?}",
            action("absent.fits", true)
        );

        let shape = probe_file(&p("adu.fits"));
        assert_eq!((shape.width, shape.height, shape.planes), (8, 8, 1));
        assert_eq!(prepared_bytes(&shape, &action("adu.fits", true)), 8 * 8 * 4);
        let mosaic = probe_file(&p("mosaic.fits"));
        assert_eq!(
            prepared_bytes(&mosaic, &action("mosaic.fits", true)),
            3 * 8 * 8 * 4,
            "the debayered copy only"
        );
        assert_eq!(
            prepared_bytes(&probe_file(&p("mono.fits")), &PrepareAction::InPlace),
            0
        );
    }
}

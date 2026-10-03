//! Prepare — stage 1 of a run whose frames are already calibrated (spec
//! 2026-10-02 §5.1, §13; plan 6b-2 C12): a project's landed replicas and an
//! attested frame set's lights. A file the plane reader decodes as unit-
//! domain float32/64 is used IN PLACE; an integer FITS or an XISF is
//! converted to float32 FITS; a CFA mosaic is debayered when `debayerOsc`
//! is on; a file no reader here can decode is excluded "unsupported file".
//! The classifier reads a file from its header alone; the plan's space
//! estimate and warnings and the run read the same answer. The writers below
//! never touch the file they read.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Context;

use crate::calibration_library::light_resolve::card_from_kv;
use crate::export::calibrated_generator::{debayer_plane, debayered_cards};
use crate::fits_parser::stored_header::parse_stored_header_keys;
use crate::fits_writer::{write_fits_f32_with, Card, CardValue, Durability};
use crate::integration::cfa::CfaGeometry;
use crate::integration::plane_reader::PlaneReader;
use crate::integration::IntegrationError;
use crate::models::FileFormat;
use crate::stacking::master_cards::ATH_STK_VERSION;
use crate::stacking::origin::cfa_geometry_of_file;
use crate::stacking::project_source::{is_xisf, xisf_geometry};
use crate::stacking::register::writer::{source_cards_from_file, REGISTERED_COPY_THROUGH};

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
        let distributed = ["path(", "url("].iter().any(|scheme| {
            xml.contains(&format!("location=\"{scheme}"))
                || xml.contains(&format!("location='{scheme}"))
        });
        if distributed {
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

/// `astroimage::ImageConverter::read_raw` returns an XISF's samples
/// bounds-normalized and multiplied by this (the reader's documented
/// contract); Prepare divides it back out, which lands within one ulp of the
/// stored value.
pub const XISF_READ_SCALE: f64 = 65535.0;

/// The copy-through cards (`REGISTERED_COPY_THROUGH`) of a FITS or XISF
/// file, from its own header — no catalog.
pub fn source_cards_of(path: &Path) -> anyhow::Result<Vec<Card>> {
    if !is_xisf(path) {
        return source_cards_from_file(path);
    }
    let raw = crate::fits_parser::read_xisf_header(path)
        .with_context(|| format!("reading the XISF header of {}", path.display()))?;
    let keys = parse_stored_header_keys(FileFormat::XISF, &String::from_utf8_lossy(&raw));
    let mut cards: Vec<Card> = keys
        .iter()
        .filter(|(k, _)| REGISTERED_COPY_THROUGH.contains(&k.to_ascii_uppercase().as_str()))
        .filter_map(|(k, v)| card_from_kv(k, v))
        .collect();
    cards.sort_by(|a, b| a.keyword.cmp(&b.keyword));
    Ok(cards)
}

/// The header of a file Prepare writes (plan 6b-2 C12):
/// - the source's copy-through cards;
/// - the CFA cards when the output is still a mosaic;
/// - `IMAGETYP = 'LIGHT'`;
/// - `ATH_STK = T` (a stacking intermediate, which the scanner never catalogs);
/// - `ATH_STKV`;
/// - `ATH_PREP` (`converted` | `debayered`).
///
/// It never carries `CALSTAT`, `ATH_CSRC`, `ATH_PRJ` or `ATH_FILT`: none of
/// them is in the copy-through list.
pub fn prepare_cards(
    source: &[Card],
    mosaic: Option<CfaGeometry>,
    prepared: &str,
) -> anyhow::Result<Vec<Card>> {
    let mut cards: Vec<Card> = source
        .iter()
        .filter(|c| REGISTERED_COPY_THROUGH.contains(&c.keyword.as_str()))
        .cloned()
        .collect();
    if let Some(g) = mosaic {
        cards.push(Card::new(
            "BAYERPAT",
            CardValue::Str(g.pattern.as_str().to_string()),
        )?);
        cards.push(Card::new("XBAYROFF", CardValue::Integer(g.xoff))?);
        cards.push(Card::new("YBAYROFF", CardValue::Integer(g.yoff))?);
    }
    cards.push(Card::new("IMAGETYP", CardValue::Str("LIGHT".into()))?);
    cards.push(
        Card::new("ATH_STK", CardValue::Logical(true))?
            .with_comment("stacking intermediate; never cataloged"),
    );
    cards.push(Card::new("ATH_STKV", CardValue::Integer(ATH_STK_VERSION))?);
    cards.push(
        Card::new("ATH_PREP", CardValue::Str(prepared.to_string()))?
            .with_comment("how Prepare produced this file"),
    );
    Ok(cards)
}

/// `(width, height, planes, planar data)` of `src` in the unit domain, read
/// per `read`: a FITS through the plane reader (physical values, then ÷ the
/// divisor for an integer file), an XISF through `read_raw` ÷
/// [`XISF_READ_SCALE`].
fn read_unit_planes(
    src: &Path,
    read: &PrepareRead,
) -> anyhow::Result<(usize, usize, usize, Vec<f32>)> {
    match read {
        PrepareRead::InPlaceFits | PrepareRead::ScaledFits { .. } => {
            let reader =
                PlaneReader::open(src).with_context(|| format!("opening {}", src.display()))?;
            let (w, h, c) = (reader.width(), reader.height(), reader.channels());
            let mut data = Vec::with_capacity(w * h * c);
            for plane in 0..c {
                data.extend(
                    reader
                        .read_plane(plane)
                        .with_context(|| format!("reading {}", src.display()))?,
                );
            }
            if let PrepareRead::ScaledFits { divisor } = read {
                for v in &mut data {
                    *v = (*v as f64 / divisor) as f32;
                }
            }
            Ok((w, h, c, data))
        }
        PrepareRead::Xisf => {
            let (meta, pixels) = astroimage::ImageConverter::read_raw(src)
                .map_err(|e| anyhow::anyhow!("{}: {e:#}", src.display()))?;
            let data: Vec<f32> = match pixels {
                astroimage::PixelData::Float32(v) => v
                    .into_iter()
                    .map(|x| (x as f64 / XISF_READ_SCALE) as f32)
                    .collect(),
                // `read_raw` returns `Float32` for every XISF sample format
                // (review M21); this arm exists only because the enum has two
                // variants.
                astroimage::PixelData::Uint16(v) => v
                    .into_iter()
                    .map(|x| (x as f64 / XISF_READ_SCALE) as f32)
                    .collect(),
            };
            Ok((meta.width, meta.height, meta.channels, data))
        }
    }
}

/// Read `src` into the unit domain per `read` and write it as a float32 FITS
/// at `dest` with `cards` (atomic: temp file + rename). Returns `(width,
/// height, planes)`.
pub fn convert_to_float_fits(
    src: &Path,
    dest: &Path,
    read: &PrepareRead,
    cards: &[Card],
    durability: Durability,
) -> anyhow::Result<(usize, usize, usize)> {
    let (w, h, c, data) = read_unit_planes(src, read)?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_fits_f32_with(dest, w, h, c, &data, cards, durability)
        .map_err(|e| anyhow::anyhow!("writing {}: {e}", dest.display()))?;
    Ok((w, h, c))
}

/// Debayer a one-plane float FITS mosaic at `src` into a three-plane float32
/// FITS at `dest` (plan 6b-2 C12), through the generator's own
/// [`debayer_plane`] with `geom`'s phase. `cards` are written as given (the
/// caller applies [`debayered_cards`]). It checks `cancel` before the
/// debayer, the expensive part, and fails with
/// [`IntegrationError::Cancelled`]. Returns the written file's size.
pub fn debayer_file(
    src: &Path,
    dest: &Path,
    geom: CfaGeometry,
    cards: &[Card],
    durability: Durability,
    pool: Option<&Arc<rayon::ThreadPool>>,
    cancel: &AtomicBool,
) -> anyhow::Result<u64> {
    let (w, h, mosaic) = {
        let reader =
            PlaneReader::open(src).with_context(|| format!("opening {}", src.display()))?;
        if reader.channels() != 1 {
            anyhow::bail!(
                "{}: a {}-plane image is not a CFA mosaic",
                src.display(),
                reader.channels()
            );
        }
        let plane = reader
            .read_plane(0)
            .with_context(|| format!("reading {}", src.display()))?;
        (reader.width(), reader.height(), plane)
    };
    if cancel.load(Ordering::Relaxed) {
        return Err(IntegrationError::Cancelled.into());
    }
    let rgb = debayer_plane(&mosaic, w, h, geom, pool);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_fits_f32_with(dest, w, h, 3, &rgb, cards, durability)
        .map_err(|e| anyhow::anyhow!("writing {}: {e}", dest.display()))?;
    Ok(std::fs::metadata(dest)?.len())
}

/// One frame's Prepare work, resolved on the run thread: no catalog, no
/// `RunContext`.
#[derive(Debug, Clone)]
pub struct PrepareJob {
    /// The file Prepare reads: the landed replica, or an attested light.
    pub src: PathBuf,
    pub shape: FileShape,
    pub action: PrepareAction,
    /// `prepared/<group>/p_<stem>.fits`, for a converted copy.
    pub converted_out: PathBuf,
    /// `prepared/<group>/p_<stem>_d.fits`, for a debayered copy.
    pub debayered_out: PathBuf,
    pub durability: Durability,
}

/// What Prepare hands on for one frame.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedFiles {
    /// The file stage 1 hands on (the `calibrated` artifact).
    pub calibrated: PathBuf,
    /// The CFA mosaic the debayer read (the `calibrated_mosaic` artifact
    /// when Bayer drizzle wants one): the file itself for a float mosaic,
    /// its converted copy otherwise. `None` when nothing was debayered.
    pub mosaic: Option<PathBuf>,
    /// `in_place` | `converted` | `debayered` — the `prepared` log field.
    pub prepared: &'static str,
}

/// Prepare one frame (plan 6b-2 C12):
/// - in place: nothing is written;
/// - converted: the unit-domain copy;
/// - debayered: from the file itself (a float mosaic) or from its converted
///   copy.
///
/// Never writes, renames or deletes `job.src`.
pub fn execute_prepare(
    job: &PrepareJob,
    pool: Option<&Arc<rayon::ThreadPool>>,
    cancel: &AtomicBool,
) -> anyhow::Result<PreparedFiles> {
    match &job.action {
        PrepareAction::InPlace => Ok(PreparedFiles {
            calibrated: job.src.clone(),
            mosaic: None,
            prepared: "in_place",
        }),
        PrepareAction::Unsupported(why) => anyhow::bail!("unsupported file: {why}"),
        PrepareAction::Write { read, debayer } => {
            let source = source_cards_of(&job.src)?;
            let base = if *read == PrepareRead::InPlaceFits {
                job.src.clone()
            } else {
                let mosaic = if job.shape.planes == 1 {
                    job.shape.cfa
                } else {
                    None
                };
                let cards = prepare_cards(&source, mosaic, "converted")?;
                convert_to_float_fits(&job.src, &job.converted_out, read, &cards, job.durability)?;
                job.converted_out.clone()
            };
            match debayer {
                None => Ok(PreparedFiles {
                    calibrated: base,
                    mosaic: None,
                    prepared: "converted",
                }),
                Some(geom) => {
                    let mut cards = prepare_cards(&source, None, "debayered")?;
                    debayered_cards(&mut cards)?;
                    debayer_file(
                        &base,
                        &job.debayered_out,
                        *geom,
                        &cards,
                        job.durability,
                        pool,
                        cancel,
                    )?;
                    Ok(PreparedFiles {
                        calibrated: job.debayered_out.clone(),
                        mosaic: Some(base),
                        prepared: "debayered",
                    })
                }
            }
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
        let single = "<?xml version='1.0'?><xisf version='1.0'><Image geometry='8:8:1' sampleFormat='Float32' location='url(http://x/y)'/></xisf>";
        let mut single_bytes = b"XISF0100".to_vec();
        single_bytes.extend_from_slice(&(single.len() as u32).to_le_bytes());
        single_bytes.extend_from_slice(&[0u8; 4]);
        single_bytes.extend_from_slice(single.as_bytes());
        std::fs::write(p("dist1.xisf"), single_bytes).unwrap();

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
        assert_eq!(
            action("dist1.xisf", true),
            PrepareAction::Unsupported("distributed XISF".into()),
            "single-quoted attributes"
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

    fn stamp_cards() -> Vec<Card> {
        let mut cards = mosaic_cards();
        for (k, v) in [
            ("FILTER", "L"),
            ("ROWORDER", "TOP-DOWN"),
            ("CALSTAT", "BDF"),
            ("ATH_CSRC", "u1"),
            ("ATH_PRJ", "p1"),
            ("ATH_FILT", "Lum"),
        ] {
            cards.push(Card::new(k, CardValue::Str(v.into())).unwrap());
        }
        cards.push(Card::new("EXPTIME", CardValue::Real(300.0)).unwrap());
        cards
    }

    /// Plan 6b-2 C12 (Review Focus 3): an integer FITS lands in the unit
    /// domain through the generator's own divisor. Its header keeps the
    /// copy-through cards and, while it is still a mosaic, the CFA cards. It
    /// never carries a calibration or project stamp.
    #[test]
    fn an_integer_fits_converts_into_the_unit_domain() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("adu.fits");
        // Stored i16 + BZERO 32768: physical 0, 1000, …, 63000.
        let data: Vec<i16> = (0..64).map(|i| (i * 1000 - 32768) as i16).collect();
        crate::stacking::test_fixtures::write_fits_i16(&src, 8, 8, &data, &stamp_cards());
        let shape = probe_file(&src);
        let cards = prepare_cards(&source_cards_of(&src).unwrap(), shape.cfa, "converted").unwrap();
        let dest = dir.path().join("out").join("p_adu.fits");
        let dims = convert_to_float_fits(
            &src,
            &dest,
            &PrepareRead::ScaledFits { divisor: 65535.0 },
            &cards,
            Durability::Durable,
        )
        .unwrap();
        assert_eq!(dims, (8, 8, 1));
        let got = PlaneReader::open(&dest).unwrap().read_plane(0).unwrap();
        for (i, v) in got.iter().enumerate() {
            let expected = ((i * 1000) as f64 / 65535.0) as f32;
            assert_eq!(v.to_bits(), expected.to_bits(), "pixel {i}");
        }
        let h = crate::fits_parser::FitsHeader::from_path(&dest).unwrap();
        for absent in ["CALSTAT", "ATH_CSRC", "ATH_PRJ", "ATH_FILT"] {
            assert_eq!(h.get_str(absent), None, "{absent} must never be copied");
        }
        assert_eq!(h.get_str("FILTER").as_deref(), Some("L"));
        assert_eq!(h.get_str("ROWORDER").as_deref(), Some("TOP-DOWN"));
        assert_eq!(h.get_f64("EXPTIME"), Some(300.0));
        assert_eq!(h.get_str("BAYERPAT").as_deref(), Some("RGGB"));
        assert_eq!(h.get_i32("XBAYROFF"), Some(0));
        assert_eq!(h.get_str("IMAGETYP").as_deref(), Some("LIGHT"));
        assert_eq!(h.get_str("ATH_STK").as_deref(), Some("T"));
        assert_eq!(h.get_i32("ATH_STKV"), Some(1));
        assert_eq!(h.get_str("ATH_PREP").as_deref(), Some("converted"));
    }

    /// Plan 6b-2 C12: an XISF is read through `read_raw` (×65535 by
    /// contract) and divided back, so every sample lands within one ulp of the
    /// value stored; its copy-through cards come from the XISF header.
    #[test]
    fn an_xisf_converts_into_the_unit_domain_within_one_ulp() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("x.xisf");
        let values: Vec<f32> = (0..64).map(|i| i as f32 / 64.0).collect();
        write_xisf_f32(&src, 8, 8, 1, &values, &stamp_cards()).unwrap();
        let cards = prepare_cards(&source_cards_of(&src).unwrap(), None, "converted").unwrap();
        let dest = dir.path().join("p_x.fits");
        convert_to_float_fits(&src, &dest, &PrepareRead::Xisf, &cards, Durability::Durable)
            .unwrap();
        let got = PlaneReader::open(&dest).unwrap().read_plane(0).unwrap();
        for (i, (g, v)) in got.iter().zip(&values).enumerate() {
            let ulps = (g.to_bits() as i64 - v.to_bits() as i64).abs();
            assert!(ulps <= 1, "pixel {i}: {g} vs {v} ({ulps} ulp)");
        }
        let h = crate::fits_parser::FitsHeader::from_path(&dest).unwrap();
        assert_eq!(h.get_str("FILTER").as_deref(), Some("L"));
        assert_eq!(h.get_str("CALSTAT"), None);
        assert_eq!(h.get_str("ATH_PRJ"), None);
    }

    /// Plan 6b-2 C12: what `execute_prepare` hands on for each action — the
    /// file itself in place; a debayered copy plus the mosaic it came from
    /// (the file itself for a float mosaic, its converted copy for an integer
    /// one); an unsupported file is an error; a cancel before the debayer
    /// writes nothing.
    #[test]
    fn execute_prepare_hands_on_the_right_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n);
        let out = dir.path().join("prepared");
        let mono: Vec<f32> = (0..256).map(|i| (i % 7) as f32 / 10.0).collect();
        write_fits_f32(&p("mono.fits"), 16, 16, 1, &mono, &[]).unwrap();
        write_fits_f32(&p("mosaic.fits"), 16, 16, 1, &mono, &stamp_cards()).unwrap();
        let adu: Vec<i16> = (0..256).map(|i| (i * 100 - 32768) as i16).collect();
        crate::stacking::test_fixtures::write_fits_i16(
            &p("adu_mosaic.fits"),
            16,
            16,
            &adu,
            &stamp_cards(),
        );
        write_empty_primary(&p("tile.fits"));
        let job = |name: &str| {
            let src = p(name);
            let stem = Path::new(name)
                .file_stem()
                .unwrap()
                .to_str()
                .unwrap()
                .to_string();
            let shape = probe_file(&src);
            PrepareJob {
                action: prepare_action(&shape, true),
                shape,
                src,
                converted_out: out.join(format!("p_{stem}.fits")),
                debayered_out: out.join(format!("p_{stem}_d.fits")),
                durability: Durability::Volatile,
            }
        };
        let go = AtomicBool::new(false);

        assert_eq!(
            execute_prepare(&job("mono.fits"), None, &go).unwrap(),
            PreparedFiles {
                calibrated: p("mono.fits"),
                mosaic: None,
                prepared: "in_place"
            }
        );

        let float = execute_prepare(&job("mosaic.fits"), None, &go).unwrap();
        assert_eq!(
            float,
            PreparedFiles {
                calibrated: out.join("p_mosaic_d.fits"),
                mosaic: Some(p("mosaic.fits")),
                prepared: "debayered",
            }
        );
        let h = crate::fits_parser::FitsHeader::from_path(&float.calibrated).unwrap();
        assert_eq!(h.get_i32("NAXIS3"), Some(3));
        assert_eq!(h.get_str("BAYERPAT"), None);
        assert_eq!(h.get_str("ATH_CDBM").as_deref(), Some("VNG"));
        assert_eq!(h.get_str("ATH_PREP").as_deref(), Some("debayered"));
        assert_eq!(h.get_str("ATH_STK").as_deref(), Some("T"));
        assert_eq!(h.get_str("CALSTAT"), None);

        let integer = execute_prepare(&job("adu_mosaic.fits"), None, &go).unwrap();
        assert_eq!(integer.calibrated, out.join("p_adu_mosaic_d.fits"));
        assert_eq!(integer.mosaic, Some(out.join("p_adu_mosaic.fits")));
        let h = crate::fits_parser::FitsHeader::from_path(&out.join("p_adu_mosaic.fits")).unwrap();
        assert_eq!(
            h.get_str("BAYERPAT").as_deref(),
            Some("RGGB"),
            "the converted copy is still a mosaic"
        );
        assert_eq!(h.get_str("ATH_PREP").as_deref(), Some("converted"));

        let err = execute_prepare(&job("tile.fits"), None, &go).unwrap_err();
        assert!(
            format!("{err:#}").starts_with("unsupported file: "),
            "{err:#}"
        );

        let stop = AtomicBool::new(true);
        let mut cancelled = job("mosaic.fits");
        cancelled.debayered_out = out.join("never.fits");
        let err = execute_prepare(&cancelled, None, &stop).unwrap_err();
        assert!(matches!(
            err.downcast_ref::<IntegrationError>(),
            Some(IntegrationError::Cancelled)
        ));
        assert!(!out.join("never.fits").exists());
    }
}

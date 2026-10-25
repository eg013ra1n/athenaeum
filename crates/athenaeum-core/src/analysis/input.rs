//! Raw frame input for the one star path (Plan C, Task C2).
//!
//! Reads a FITS / XISF file through `astroimage::ImageConverter::read_raw` and states what the pixels ARE: the
//! planes as `f32` in the file's own ADU, and the [`star_measure::Domain::RawAdu`] the module's saturation test and
//! SNR shot term read (`clip`, `full_scale`, `gain_e_per_adu`). Rulings PC-2 / PC-3 (amended by R-PC-1).
//!
//! BITPIX 16 / BZERO 0 (R-PC-1): the rule lives in the rustafits 1.6 reader. More than 1 % negative samples means an
//! offset-written unsigned file and the reader returns `stored + 32768`; otherwise negatives clamp to 0. This module
//! only picks the full scale of the plane it is handed.

use std::path::Path;

use anyhow::{bail, Context, Result};
use astroimage::{BayerPattern, ImageConverter, PixelData};

use crate::fits_parser::{self, stored_header::parse_stored_header_keys, FitsHeader};
use crate::fits_writer::OutputFormat;
use crate::models::FileFormat;
use crate::star_measure::Domain;

/// ADU scale of a unit-range float file and of the XISF reader's float contract.
const UNIT_SCALE: f32 = 65535.0;

/// The pixel planes of a raw frame, in the file's ADU, in FILE row order: `flip_vertical` is NOT applied. The reader
/// never adjusts `bayer_pattern` for ROWORDER or a flip, so a CFA `pattern` addresses the file's row order, with the
/// file's XBAYROFF / YBAYROFF folded in (R-PC-4): the rustafits FITS reader folds them, its XISF reader passes the
/// pattern through and [`fold_bayer_offsets`] folds them here. A consumer that flips an even-height mosaic must swap
/// the pattern itself (RGGB becomes GBRG).
#[derive(Debug, Clone)]
pub enum RawPlanes {
    Mono(Vec<f32>),
    /// A Bayer mosaic on the native grid.
    Cfa {
        mosaic: Vec<f32>,
        pattern: BayerPattern,
    },
    /// A debayered file, planar R, G, B.
    Rgb([Vec<f32>; 3]),
}

/// One raw frame: planes, size, the domain they are in, and the file's channel count.
#[derive(Debug, Clone)]
pub struct RawFrame {
    pub planes: RawPlanes,
    pub w: usize,
    pub h: usize,
    pub domain: Domain,
    /// The file is bottom-up (no `ROWORDER = TOP-DOWN`); the planes are still in file row order.
    pub flip_vertical: bool,
    pub channels: usize,
}

impl RawFrame {
    /// A debayered file is measured on its G plane alone: free R and B (about two thirds of the frame's memory) once the
    /// read is done. A no-op for mono and mosaic frames; the measurement never reads the emptied planes.
    pub fn drop_unmeasured_planes(&mut self) {
        if let RawPlanes::Rgb(planes) = &mut self.planes {
            planes[0] = Vec::new();
            planes[2] = Vec::new();
        }
    }
}

/// The header facts the domain needs.
#[derive(Debug, Default)]
struct HeaderFacts {
    bitpix: Option<i32>,
    bzero: f64,
    bscale: f64,
    datamax: Option<f64>,
    egain: Option<f64>,
    xbayroff: i64,
    ybayroff: i64,
}

/// A header offset as an integer; absent or unreadable is 0.
fn offset(v: Option<f64>) -> i64 {
    v.filter(|x| x.is_finite()).map_or(0, |x| x.round() as i64)
}

/// The pattern with the file's `XBAYROFF` / `YBAYROFF` folded in (R-PC-4), the repo's convention
/// (`integration/cfa.rs` `cfa_channel_at`): offsets mod 2, an odd x offset swaps the columns of the 2x2 cell, an odd y
/// offset swaps its rows. The rustafits FITS reader folds them itself (rustafits issue #7), its XISF reader does not:
/// applied to XISF only, where without it an odd offset flips the green parity.
pub fn fold_bayer_offsets(pattern: BayerPattern, xoff: i64, yoff: i64) -> BayerPattern {
    use BayerPattern::{Bggr, Gbrg, Grbg, None, Rggb};
    let swap_cols = |p| match p {
        Rggb => Grbg,
        Grbg => Rggb,
        Bggr => Gbrg,
        Gbrg => Bggr,
        None => None,
    };
    let swap_rows = |p| match p {
        Rggb => Gbrg,
        Gbrg => Rggb,
        Bggr => Grbg,
        Grbg => Bggr,
        None => None,
    };
    let mut p = pattern;
    if xoff.rem_euclid(2) == 1 {
        p = swap_cols(p);
    }
    if yoff.rem_euclid(2) == 1 {
        p = swap_rows(p);
    }
    p
}

fn num(map: &std::collections::HashMap<String, String>, key: &str) -> Option<f64> {
    map.get(key)
        .and_then(|v| v.trim().trim_matches('\'').trim().parse::<f64>().ok())
}

fn header_facts(path: &Path, xisf: bool) -> Result<HeaderFacts> {
    if xisf {
        let text = fits_parser::extract_xisf_header(path)
            .with_context(|| format!("XISF header: {}", path.display()))?;
        let map = parse_stored_header_keys(FileFormat::XISF, &text);
        Ok(HeaderFacts {
            bitpix: None,
            bzero: 0.0,
            bscale: 1.0,
            datamax: num(&map, "DATAMAX"),
            egain: num(&map, "EGAIN"),
            xbayroff: offset(num(&map, "XBAYROFF")),
            ybayroff: offset(num(&map, "YBAYROFF")),
        })
    } else {
        let h = FitsHeader::from_path(path)?;
        Ok(HeaderFacts {
            bitpix: h.get_i32("BITPIX"),
            bzero: h.get_f64("BZERO").unwrap_or(0.0),
            bscale: h.get_f64("BSCALE").unwrap_or(1.0),
            datamax: h.get_f64("DATAMAX"),
            egain: h.get_f64("EGAIN"),
            // The rustafits FITS reader folds XBAYROFF / YBAYROFF into `bayer_pattern` (issue #7): folding again
            // would undo it.
            xbayroff: 0,
            ybayroff: 0,
        })
    }
}

/// Read `path` as a raw frame (PC-2, PC-3 as amended by R-PC-1).
pub fn read_raw_frame(path: &Path) -> Result<RawFrame> {
    read_inner(path).map_err(|e| {
        tracing::error!(path = %path.display(), error = %format!("{e:#}"), "raw frame read failed");
        e
    })
}

fn read_inner(path: &Path) -> Result<RawFrame> {
    let xisf = OutputFormat::from_path(path) == OutputFormat::Xisf;
    let facts = header_facts(path, xisf)?;
    let (meta, pixels) =
        ImageConverter::read_raw(path).map_err(|e| anyhow::anyhow!("{}: {e:#}", path.display()))?;
    let (w, h, channels) = (meta.width, meta.height, meta.channels);
    let n = w * h * channels;

    // (samples, full scale, whether the plane was scaled by 65535 from a unit range)
    let (mut samples, full_scale, scaled): (Vec<f32>, f64, bool) = match pixels {
        PixelData::Uint16(v) => {
            if v.len() != n {
                bail!(
                    "{}: {} samples for a {w}x{h}x{channels} image",
                    path.display(),
                    v.len()
                );
            }
            match (facts.bitpix, facts.bzero == 0.0 && facts.bscale == 1.0) {
                (Some(16), true) => {
                    let (s, full) = read_bitpix16_bzero0(path, &v);
                    (s, full, false)
                }
                // The reader scales 8-bit samples by 256 into the u16 plane: undo it, the plane is in file ADU (R-PC-2).
                (Some(8), _) => (
                    v.iter().map(|&x| f32::from(x) / 256.0).collect(),
                    255.0,
                    false,
                ),
                // BITPIX 16 with BSCALE != 1 or BZERO not in {0, 32768} arrives clamped to [0, 65535] by the reader:
                // negatives are lost and the 1 % rule cannot run.
                _ => (v.iter().map(|&x| f32::from(x)).collect(), 65535.0, false),
            }
        }
        PixelData::Float32(mut v) => {
            if v.len() != n {
                bail!(
                    "{}: {} samples for a {w}x{h}x{channels} image",
                    path.display(),
                    v.len()
                );
            }
            let max = v
                .iter()
                .copied()
                .filter(|x| x.is_finite())
                .fold(f32::NEG_INFINITY, f32::max);
            if xisf {
                // The reader's contract: Float32 XISF is already x65535 (UInt8 is x257 and UInt32 is >> 16 by the
                // reader, so a DATAMAX in file units is off by that factor for them; only the samples are rescaled).
                (v, finite_max_full_scale(path, max), true)
            } else if facts.bitpix == Some(32) {
                // Integer FITS wider than 16 bits takes the float rule without the unit range (R-PC-2).
                (v, finite_max_full_scale(path, max), false)
            } else if max <= 1.0 {
                for x in v.iter_mut() {
                    *x *= UNIT_SCALE;
                }
                (v, 65535.0, true)
            } else {
                (v, finite_max_full_scale(path, max), false)
            }
        }
    };

    // DATAMAX is in the unit of the file: x65535 only when the plane was scaled from a unit range and DATAMAX is one.
    let factor = match facts.datamax {
        Some(d) if scaled && d > 0.0 && d <= 1.0 => f64::from(UNIT_SCALE),
        _ => 1.0,
    };
    let clip = match facts.datamax {
        Some(d) if d.is_finite() && d > 0.0 => (d * factor).min(full_scale),
        _ => full_scale,
    };
    let gain_e_per_adu = facts.egain.filter(|g| g.is_finite() && *g > 0.0);
    let domain = Domain::RawAdu {
        clip,
        full_scale,
        gain_e_per_adu,
    };

    let planes = match channels {
        1 => match meta.bayer_pattern {
            BayerPattern::None => RawPlanes::Mono(std::mem::take(&mut samples)),
            pattern => RawPlanes::Cfa {
                mosaic: std::mem::take(&mut samples),
                pattern: fold_bayer_offsets(pattern, facts.xbayroff, facts.ybayroff),
            },
        },
        3 => {
            let plane = w * h;
            let b = samples.split_off(2 * plane);
            let g = samples.split_off(plane);
            RawPlanes::Rgb([samples, g, b])
        }
        c => bail!("{}: {c} channels, expected 1 or 3", path.display()),
    };
    Ok(RawFrame {
        planes,
        w,
        h,
        domain,
        flip_vertical: meta.flip_vertical,
        channels,
    })
}

/// Full scale of a float-valued plane: 65535 when its finite maximum fits, else that maximum with one `warn!`.
fn finite_max_full_scale(path: &Path, max: f32) -> f64 {
    if f64::from(max) > 65535.0 {
        tracing::warn!(path = %path.display(), finite_max = f64::from(max), "float file above 65535 ADU, full scale follows its maximum");
        f64::from(max)
    } else {
        65535.0
    }
}

/// BITPIX 16 / BZERO 0: the rustafits 1.6 reader already applied the R-PC-1 rule (offset or clamp). Full scale is
/// 65535 when the plane's maximum exceeds 32767, else 32767.
fn read_bitpix16_bzero0(path: &Path, plane: &[u16]) -> (Vec<f32>, f64) {
    let max = plane.iter().copied().max().unwrap_or(0);
    let full = if max > 32767 { 65535.0 } else { 32767.0 };
    if max > 32767 {
        // Only the 32768 offset can produce a value above 32767 from a BZERO 0 file (R-PC-1a).
        tracing::warn!(path = %path.display(), plane_max = max, "BITPIX 16 / BZERO 0 file read with the 32768 offset");
    } else {
        tracing::debug!(path = %path.display(), plane_max = max, full_scale = full, "BITPIX 16 / BZERO 0 full scale chosen");
    }
    (plane.iter().map(|&x| f32::from(x)).collect(), full)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::{write_xisf_f32, Card, CardValue};
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::layer::SubscriberExt;

    /// Run `f`, returning its result and every WARN event as its `name=value` fields.
    fn warn_log<T>(f: impl FnOnce() -> T) -> (T, Vec<Vec<(String, String)>>) {
        struct Fields(Vec<(String, String)>);
        impl tracing::field::Visit for Fields {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                self.0
                    .push((field.name().to_string(), format!("{value:?}")));
            }
        }
        struct Count(Arc<Mutex<Vec<Vec<(String, String)>>>>);
        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Count {
            fn on_event(
                &self,
                e: &tracing::Event<'_>,
                _: tracing_subscriber::layer::Context<'_, S>,
            ) {
                if *e.metadata().level() == tracing::Level::WARN {
                    let mut v = Fields(Vec::new());
                    e.record(&mut v);
                    self.0.lock().unwrap().push(v.0);
                }
            }
        }
        let n = Arc::new(Mutex::new(Vec::new()));
        let sub = tracing_subscriber::registry().with(Count(n.clone()));
        let out = tracing::subscriber::with_default(sub, || {
            tracing::callsite::rebuild_interest_cache();
            f()
        });
        let log = n.lock().unwrap().clone();
        (out, log)
    }

    fn warns<T>(f: impl FnOnce() -> T) -> (T, usize) {
        let (out, log) = warn_log(f);
        (out, log.len())
    }

    fn card(key: &str, val: &str) -> String {
        format!("{key:<8}= {val:>20}")
            .chars()
            .chain(std::iter::repeat(' '))
            .take(80)
            .collect()
    }

    /// A minimal FITS file: `dims` are NAXIS1.., `extra` are (keyword, value text) cards, `data` big-endian bytes.
    fn write_fits(path: &Path, bitpix: i32, dims: &[usize], extra: &[(&str, String)], data: &[u8]) {
        let mut cards = vec![
            format!("{:<80}", format!("SIMPLE  = {:>20}", "T")),
            card("BITPIX", &bitpix.to_string()),
            card("NAXIS", &dims.len().to_string()),
        ];
        for (i, d) in dims.iter().enumerate() {
            cards.push(card(&format!("NAXIS{}", i + 1), &d.to_string()));
        }
        for (k, v) in extra {
            cards.push(card(k, v));
        }
        cards.push(format!("{:<80}", "END"));
        let mut bytes: Vec<u8> = cards.concat().into_bytes();
        bytes.resize(bytes.len().div_ceil(2880) * 2880, b' ');
        bytes.extend_from_slice(data);
        bytes.resize(bytes.len().div_ceil(2880) * 2880, 0);
        std::fs::write(path, bytes).unwrap();
    }

    fn be16(v: &[i16]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_be_bytes()).collect()
    }
    fn bef32(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_be_bytes()).collect()
    }
    fn mono(f: &RawFrame) -> &[f32] {
        match &f.planes {
            RawPlanes::Mono(v) => v,
            other => panic!("expected Mono, got {other:?}"),
        }
    }
    fn raw(f: &RawFrame) -> (f64, f64, Option<f64>) {
        match f.domain {
            Domain::RawAdu {
                clip,
                full_scale,
                gain_e_per_adu,
            } => (clip, full_scale, gain_e_per_adu),
            d => panic!("expected RawAdu, got {d:?}"),
        }
    }

    #[test]
    fn bitpix16_bzero0_reads_signed() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.fits");
        let mut v = vec![100i16; 1000];
        for x in v.iter_mut().take(5) {
            *x = -5;
        }
        write_fits(&p, 16, &[40, 25], &[], &be16(&v));
        let f = read_raw_frame(&p).unwrap();
        // The 1.6 reader clamps the few negatives to 0.
        assert_eq!(mono(&f)[0], 0.0);
        assert_eq!(mono(&f)[10], 100.0);
        let (clip, full, gain) = raw(&f);
        assert_eq!((clip, full, gain), (32767.0, 32767.0, None));
    }

    #[test]
    fn bitpix16_bzero32768_reads_unsigned() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.fits");
        let stored = (40000i32 - 32768) as i16;
        write_fits(
            &p,
            16,
            &[2, 2],
            &[("BZERO", "32768".into())],
            &be16(&[stored; 4]),
        );
        let f = read_raw_frame(&p).unwrap();
        assert!(mono(&f).iter().all(|&x| x == 40000.0));
        assert_eq!(raw(&f).1, 65535.0);
    }

    #[test]
    fn bitpix16_bzero0_holding_offset_data_reads_with_the_32768_offset() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.fits");
        write_fits(&p, 16, &[3, 1], &[], &be16(&[-30000, 0, 32767]));
        let (f, log) = warn_log(|| read_raw_frame(&p).unwrap());
        assert_eq!(mono(&f), &[2768.0, 32768.0, 65535.0]);
        assert_eq!(raw(&f).1, 65535.0);
        assert_eq!(log.len(), 1, "one warn per offset-read file: {log:?}");
        assert!(
            log[0].iter().any(|(k, v)| k == "path" && v.contains("a.fits")),
            "the warn names the path: {:?}",
            log[0]
        );
        assert!(log[0].iter().any(|(k, v)| k == "plane_max" && v == "65535"));
    }

    #[test]
    fn a_unit_float_fits_is_scaled_to_adu() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.fits");
        write_fits(
            &p,
            -32,
            &[2, 2],
            &[("DATAMAX", "0.5".into())],
            &bef32(&[0.0, 0.2, 0.5, 0.8]),
        );
        let f = read_raw_frame(&p).unwrap();
        assert_eq!(mono(&f)[3], 0.8f32 * 65535.0);
        let (clip, full, _) = raw(&f);
        assert_eq!(full, 65535.0);
        assert_eq!(clip, 0.5 * 65535.0);
    }

    #[test]
    fn an_adu_float_fits_keeps_its_values() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.fits");
        write_fits(&p, -32, &[2, 2], &[], &bef32(&[0.0, 10.0, 500.0, 42000.0]));
        let (f, n) = warns(|| read_raw_frame(&p).unwrap());
        assert_eq!((mono(&f)[3], raw(&f).1, n), (42000.0, 65535.0, 0));
        write_fits(&p, -32, &[2, 2], &[], &bef32(&[0.0, 10.0, 500.0, 70000.0]));
        let (f, n) = warns(|| read_raw_frame(&p).unwrap());
        assert_eq!((mono(&f)[3], raw(&f).1, n), (70000.0, 70000.0, 1));
    }

    #[test]
    fn datamax_sets_clip_and_egain_sets_gain() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.fits");
        let v = be16(&[10, 20, 30, 40]);
        write_fits(
            &p,
            16,
            &[2, 2],
            &[
                ("BZERO", "32768".into()),
                ("DATAMAX", "60000".into()),
                ("EGAIN", "0.8".into()),
            ],
            &v,
        );
        let (clip, full, gain) = raw(&read_raw_frame(&p).unwrap());
        assert_eq!((clip, full, gain), (60000.0, 65535.0, Some(0.8)));
        // A DATAMAX above full scale is capped at it.
        write_fits(&p, 16, &[2, 2], &[("DATAMAX", "60000".into())], &v);
        let (clip, full, _) = raw(&read_raw_frame(&p).unwrap());
        assert_eq!((clip, full), (32767.0, 32767.0));
        write_fits(&p, 16, &[2, 2], &[], &v);
        let (clip, full, gain) = raw(&read_raw_frame(&p).unwrap());
        assert_eq!((clip, full, gain), (32767.0, 32767.0, None));
    }

    #[test]
    fn xisf_uint16_and_float32_read_at_full_scale_65535() {
        let dir = tempfile::tempdir().unwrap();
        // Float32 through the writer: unit samples, x65535 by the reader's contract.
        let pf = dir.path().join("f.xisf");
        let cards = vec![
            Card::new("EGAIN", CardValue::Real(1.5)).unwrap(),
            Card::new("DATAMAX", CardValue::Real(0.9)).unwrap(),
        ];
        write_xisf_f32(&pf, 2, 2, 1, &[0.0, 0.25, 0.5, 1.0], &cards).unwrap();
        let f = read_raw_frame(&pf).unwrap();
        assert_eq!(mono(&f)[3], 65535.0);
        let (clip, full, gain) = raw(&f);
        assert_eq!((full, gain), (65535.0, Some(1.5)));
        assert!((clip - 0.9 * 65535.0).abs() < 1e-6);
        // UInt16 attachment, hand-written monolithic file.
        let pu = dir.path().join("u.xisf");
        let samples: [u16; 4] = [0, 1000, 40000, 65535];
        let xml = |pos: usize| {
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><xisf version=\"1.0\" xmlns=\"http://www.pixinsight.com/xisf\">\
<Image geometry=\"2:2:1\" sampleFormat=\"UInt16\" colorSpace=\"Gray\" byteOrder=\"little\" location=\"attachment:{pos:08}:8\"/></xisf>"
            )
        };
        let len = xml(0).len();
        let mut bytes = b"XISF0100".to_vec();
        bytes.extend_from_slice(&(len as u32).to_le_bytes());
        bytes.extend_from_slice(&[0u8; 4]);
        bytes.extend_from_slice(xml(16 + len).as_bytes());
        for s in samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(&pu, bytes).unwrap();
        let f = read_raw_frame(&pu).unwrap();
        assert_eq!(mono(&f), &[0.0, 1000.0, 40000.0, 65535.0]);
        assert_eq!(raw(&f), (65535.0, 65535.0, None));
    }

    #[test]
    fn an_8bit_fits_reads_in_its_own_adu() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.fits");
        write_fits(
            &p,
            8,
            &[2, 2],
            &[("DATAMAX", "250".into())],
            &[0, 10, 200, 255],
        );
        let f = read_raw_frame(&p).unwrap();
        assert_eq!(mono(&f), &[0.0, 10.0, 200.0, 255.0]);
        let (clip, full, _) = raw(&f);
        assert_eq!((clip, full), (250.0, 255.0));
    }

    #[test]
    fn bitpix32_holding_16bit_data_takes_full_scale_65535() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.fits");
        let be32 = |v: &[i32]| -> Vec<u8> { v.iter().flat_map(|x| x.to_be_bytes()).collect() };
        write_fits(&p, 32, &[2, 2], &[], &be32(&[-100, 1000, 30000, 64535]));
        let (f, n) = warns(|| read_raw_frame(&p).unwrap());
        assert_eq!(mono(&f), &[-100.0, 1000.0, 30000.0, 64535.0]);
        let (clip, full, _) = raw(&f);
        assert_eq!((clip, full, n), (65535.0, 65535.0, 0));
        write_fits(&p, 32, &[2, 2], &[], &be32(&[-100, 1000, 30000, 70000]));
        let (f, n) = warns(|| read_raw_frame(&p).unwrap());
        let (clip, full, _) = raw(&f);
        assert_eq!((clip, full, n), (70000.0, 70000.0, 1));
    }

    #[test]
    fn datamax_scales_only_a_unit_range_plane() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.fits");
        // An ADU-written float file with DATAMAX 65535 is not scaled again.
        write_fits(
            &p,
            -32,
            &[2, 2],
            &[("DATAMAX", "65535".into())],
            &bef32(&[0.0, 10.0, 500.0, 42000.0]),
        );
        let (clip, full, _) = raw(&read_raw_frame(&p).unwrap());
        assert_eq!((clip, full), (65535.0, 65535.0));
        // A unit-range plane with an ADU DATAMAX is capped at full scale.
        write_fits(
            &p,
            -32,
            &[2, 2],
            &[("DATAMAX", "65535".into())],
            &bef32(&[0.0, 0.2, 0.5, 0.8]),
        );
        let (clip, full, _) = raw(&read_raw_frame(&p).unwrap());
        assert_eq!((clip, full), (65535.0, 65535.0));
    }

    #[test]
    fn a_bayer_frame_reads_as_cfa_with_its_layout() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.fits");
        write_fits(
            &p,
            16,
            &[4, 2],
            &[("BAYERPAT", "'RGGB    '".into())],
            &be16(&[1, 2, 3, 4, 5, 6, 7, 8]),
        );
        let f = read_raw_frame(&p).unwrap();
        assert_eq!(f.channels, 1);
        assert!(f.flip_vertical, "no ROWORDER means bottom-up");
        match &f.planes {
            RawPlanes::Cfa { mosaic, pattern } => {
                assert_eq!(*pattern, BayerPattern::Rggb);
                assert_eq!(mosaic, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
            }
            other => panic!("expected Cfa, got {other:?}"),
        }
    }

    /// XISF is the one format the app folds itself (the rustafits XISF reader passes the pattern through): an RGGB
    /// mosaic with `XBAYROFF = 1` reads as GRBG, as the FITS file above does.
    #[test]
    fn an_xisf_mosaic_folds_its_bayer_offsets() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.xisf");
        let cards = vec![
            Card::new("BAYERPAT", CardValue::Str("RGGB".into())).unwrap(),
            Card::new("XBAYROFF", CardValue::Integer(1)).unwrap(),
        ];
        write_xisf_f32(&p, 4, 2, 1, &[0.1; 8], &cards).unwrap();
        match read_raw_frame(&p).unwrap().planes {
            RawPlanes::Cfa { pattern, .. } => assert_eq!(pattern, BayerPattern::Grbg),
            other => panic!("expected Cfa, got {other:?}"),
        }
    }

    #[test]
    fn bayer_offsets_fold_into_the_pattern() {
        use BayerPattern::{Bggr, Gbrg, Grbg, Rggb};
        assert_eq!(fold_bayer_offsets(Rggb, 0, 0), Rggb);
        assert_eq!(fold_bayer_offsets(Rggb, 1, 0), Grbg);
        assert_eq!(fold_bayer_offsets(Rggb, 0, 1), Gbrg);
        assert_eq!(fold_bayer_offsets(Rggb, 1, 1), Bggr);
        assert_eq!(fold_bayer_offsets(Rggb, 3, -1), Bggr);
        assert_eq!(fold_bayer_offsets(Rggb, 2, -2), Rggb);
        assert_eq!(fold_bayer_offsets(Bggr, 1, 0), Gbrg);
        assert_eq!(fold_bayer_offsets(Gbrg, 0, 1), Rggb);
        // From a file: RGGB shifted one column puts green at (0, 0), so the folded pattern's green mask (x + y even)
        // picks the true green sites, which the unfolded RGGB mask (x + y odd) would miss.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.fits");
        write_fits(
            &p,
            16,
            &[4, 2],
            &[("BAYERPAT", "'RGGB    '".into()), ("XBAYROFF", "1".into())],
            &be16(&[1, 2, 3, 4, 5, 6, 7, 8]),
        );
        match read_raw_frame(&p).unwrap().planes {
            RawPlanes::Cfa { pattern, .. } => {
                assert_eq!(pattern, Grbg);
                let m = astroimage::processing::cfa::green_mask(4, 2, pattern);
                assert_eq!(m, [true, false, true, false, false, true, false, true]);
            }
            other => panic!("expected Cfa, got {other:?}"),
        }
    }

    #[test]
    fn a_three_plane_frame_reads_as_rgb() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.fits");
        write_fits(&p, 16, &[2, 1, 3], &[], &be16(&[1, 2, 10, 20, 100, 200]));
        let f = read_raw_frame(&p).unwrap();
        assert_eq!((f.channels, f.w, f.h), (3, 2, 1));
        match &f.planes {
            RawPlanes::Rgb([r, g, b]) => {
                assert_eq!(
                    (r.as_slice(), g.as_slice(), b.as_slice()),
                    (&[1.0, 2.0][..], &[10.0, 20.0][..], &[100.0, 200.0][..])
                );
            }
            other => panic!("expected Rgb, got {other:?}"),
        }
    }

    #[test]
    #[ignore = "needs STAR_TEST_DIR (the star test corpus)"]
    fn dsc5767_reads_with_the_32768_offset() {
        let dir = std::env::var("STAR_TEST_DIR").expect("STAR_TEST_DIR");
        let f = read_raw_frame(&Path::new(&dir).join("singles/_DSC5767.fit")).unwrap();
        let RawPlanes::Rgb([_, g, _]) = &f.planes else {
            panic!("expected Rgb")
        };
        let mut s = g.clone();
        s.sort_by(f32::total_cmp);
        let median = s[s.len() / 2];
        assert!((median - 3855.0).abs() <= 1.0, "G median {median}");
        assert_eq!(s[s.len() - 1], 65535.0);
        assert_eq!(raw(&f).1, 65535.0);
    }
}

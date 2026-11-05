//! The Analysis tab's seams with rustafits (port plan phase 4): the file read, the XISF fold and the frame options are
//! rustafits', so what is tested here is what crosses Athenaeum's side of the boundary: its XISF writer read by
//! rustafits' `star_measure::raw`, and its `AnalysisConfig` turned into rustafits' `FrameOptions`.

use super::analyzer::frame_options;
use super::config::{AnalysisConfig, AnalysisPsfModel};
use crate::fits_writer::{write_xisf_f32, Card, CardValue};
use crate::star_measure::analysis_mode::FrameOptions;
use crate::star_measure::raw::{read_raw_frame, RawFrame, RawPlanes};
use crate::star_measure::{Domain, PsfModel};
use astroimage::BayerPattern;

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

/// The XISF reader in rustafits folds `XBAYROFF` / `YBAYROFF` into the Bayer pattern since phase 4 (`star_measure::raw`
/// folds nothing): an RGGB mosaic with `XBAYROFF = 1` reads as GRBG, as a FITS file does. This holds the one fold across
/// Athenaeum's writer, the reader and `raw`: zero or two folds would read RGGB.
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

/// `frame_options` carries every field of the config that reaches the measurement. The defaults alone cannot show a
/// forgotten field (the config's defaults are the options' defaults), so a second config sets all three away from them.
#[test]
fn the_default_analysis_config_measures_with_the_default_frame_options() {
    assert_eq!(
        frame_options(&AnalysisConfig::default()),
        FrameOptions::default()
    );

    let config = AnalysisConfig {
        detection_threshold: 9.5,
        psf_model: AnalysisPsfModel::Auto,
        max_stars: 123,
        ..AnalysisConfig::default()
    };
    let mut want = FrameOptions::default();
    want.detection_threshold = 9.5;
    want.psf_model = PsfModel::Auto;
    want.max_stars = 123;
    assert_eq!(frame_options(&config), want);
}

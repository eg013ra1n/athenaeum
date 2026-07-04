//! Round-trip tests for `fits_writer`: writer output re-read through both existing
//! readers (`fits_parser::FitsHeader` for header cards, `astroimage::ImageConverter`
//! for pixel data), per SDD Task 14.

use athenaeum_core::fits_writer::{write_fits_f32, Card, CardValue};
// Header reader: FitsHeader::from_path(path) -> Result<FitsHeader>
//   (crates/athenaeum-core/src/fits_parser/fits_header_reader.rs:20; getters :135-149)
// Data reader: astroimage::{ImageConverter, PixelData} — ImageConverter::read_raw(path)
//   is an associated function (no `&self`), called as ImageConverter::read_raw(&path)
//   (import + call pattern per crates/athenaeum-core/src/flat_analysis.rs:37,97).

fn sample_cards() -> Vec<Card> {
    let mut cards = vec![
        Card::new("IMAGETYP", CardValue::Str("Master Dark".into())).unwrap(),
        Card::new("EXPTIME", CardValue::Real(300.0)).unwrap().with_comment("[s] exposure"),
        Card::new("GAIN", CardValue::Integer(100)).unwrap(),
        Card::new("CCD-TEMP", CardValue::Real(-10.5)).unwrap().with_comment("[degC]"),
        Card::new("ROWORDER", CardValue::Str("TOP-DOWN".into())).unwrap(),
        Card::new("ATH_SRC", CardValue::Str("u".repeat(80))).unwrap(), // forces CONTINUE
    ];
    // Card::history_cards now returns Result<Vec<Card>, FitsWriteError> (fix round hardened it).
    cards.extend(Card::history_cards("integrated by athenaeum test").unwrap());
    cards
}

#[test]
fn header_roundtrip_through_fits_parser() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rt.fits");
    let data: Vec<f32> = (0..12).map(|i| i as f32 / 3.0).collect();
    write_fits_f32(&path, 4, 3, 1, &data, &sample_cards()).unwrap();

    let header = athenaeum_core::fits_parser::FitsHeader::from_path(&path).unwrap();
    assert_eq!(header.get_str("IMAGETYP").as_deref(), Some("Master Dark"));
    assert_eq!(header.get_f64("EXPTIME"), Some(300.0));
    assert_eq!(header.get_i32("GAIN"), Some(100));
    assert_eq!(header.get_f64("CCD-TEMP"), Some(-10.5));
    assert_eq!(
        header.get_str("ATH_SRC").as_deref(),
        Some("u".repeat(80).as_str()),
        "CONTINUE chain must reassemble"
    );

    // Block-size verification: header + data must each be a whole number of 2880-byte blocks.
    let file_len = std::fs::metadata(&path).unwrap().len();
    assert_eq!(file_len % 2880, 0, "written file must be a multiple of the FITS block size");
}

#[test]
fn data_roundtrip_through_rustafits_bit_exact_incl_nan() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rt2.fits");
    let mut data: Vec<f32> = (0..64).map(|i| (i as f32).sin()).collect();
    data[7] = f32::NAN;
    write_fits_f32(
        &path,
        8,
        8,
        1,
        &data,
        &[Card::new("ROWORDER", CardValue::Str("TOP-DOWN".into())).unwrap()], // suppress reader flip
    )
    .unwrap();

    let (_meta, pixels) = astroimage::ImageConverter::read_raw(&path).unwrap();
    let read = match pixels {
        astroimage::PixelData::Float32(v) => v,
        astroimage::PixelData::Uint16(_) => panic!("expected Float32, got Uint16 (PixelData has no Debug impl)"),
    };
    assert_eq!(read.len(), data.len());
    for (a, b) in read.iter().zip(&data) {
        assert_eq!(a.to_bits(), b.to_bits(), "bit-exact incl. NaN");
    }
}

#[test]
fn rgb_dims_and_size_validation() {
    let dir = tempfile::tempdir().unwrap();
    let ok = write_fits_f32(&dir.path().join("rgb.fits"), 2, 2, 3, &[0.0f32; 12], &[]);
    assert!(ok.is_ok());
    let bad = write_fits_f32(&dir.path().join("bad.fits"), 2, 2, 1, &[0.0f32; 3], &[]);
    assert!(bad.is_err(), "data size mismatch must fail");
}

#[test]
fn rgb_data_roundtrip_through_rustafits_bit_exact() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rgb_rt.fits");
    let (width, height, channels) = (4, 2, 3);
    let plane_size = width * height;
    // plane-major gradient: plane p, pixel i => (p*100 + i) as f32, so each
    // plane's values are distinct from the others.
    let data: Vec<f32> = (0..channels)
        .flat_map(|p| (0..plane_size).map(move |i| (p * 100 + i) as f32))
        .collect();

    write_fits_f32(
        &path,
        width,
        height,
        channels,
        &data,
        &[Card::new("ROWORDER", CardValue::Str("TOP-DOWN".into())).unwrap()],
    )
    .unwrap();

    let (meta, pixels) = astroimage::ImageConverter::read_raw(&path).unwrap();
    assert_eq!(meta.width, width);
    assert_eq!(meta.height, height);
    assert_eq!(meta.channels, channels);

    let read = match pixels {
        astroimage::PixelData::Float32(v) => v,
        astroimage::PixelData::Uint16(_) => panic!("expected Float32, got Uint16 (PixelData has no Debug impl)"),
    };
    assert_eq!(read.len(), data.len());
    for (a, b) in read.iter().zip(&data) {
        assert_eq!(a.to_bits(), b.to_bits(), "bit-exact RGB plane-major round-trip");
    }
}

#[test]
fn failed_write_preserves_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keep.fits");
    write_fits_f32(&path, 4, 3, 1, &vec![1.0f32; 12], &[]).unwrap();
    let good_len = std::fs::metadata(&path).unwrap().len();
    assert!(good_len > 0);
    // mismatched data length must fail WITHOUT touching the existing file
    let err = write_fits_f32(&path, 4, 3, 1, &vec![1.0f32; 3], &[]);
    assert!(err.is_err());
    assert_eq!(std::fs::metadata(&path).unwrap().len(), good_len, "existing file must be preserved");
    // and no stray temp file left behind
    assert!(!path.with_extension("fits.tmp").exists());
}

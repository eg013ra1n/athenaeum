//! Dev probe: rewrite a FITS frame as a monolithic XISF with the same header
//! cards, samples in the raw ADU domain (`bounds="0:65535"`), so a real
//! catalog can be seeded with a mixed FITS/XISF set for an acceptance run
//! (2026-09-18 XISF plan, Task 7 step 3b).
//!
//! Usage: `cargo run -p athenaeum-core --release --features render --example
//! xisf_convert_probe -- <src.fits> <dst.xisf> [more src/dst pairs…]`
//!
//! Pixels come through rustafits' reader (a 16-bit integer frame arrives as
//! u16, written as f32 ADU); the cards are the source header's own keywords
//! minus the structural ones the writer owns. Not a product feature — a
//! throwaway harness, exempt from the zero-print rule like every example.

use std::path::Path;

use athenaeum_core::fits_parser::stored_header::parse_stored_header_keys;
use athenaeum_core::fits_parser::FitsHeader;
use athenaeum_core::fits_writer::{write_image_f32, Card, CardValue, OutputFormat, XisfBounds};
use athenaeum_core::models::FileFormat;

/// Keywords the writer stamps itself, or that describe the FITS container
/// rather than the image.
const STRUCTURAL: &[&str] = &[
    "SIMPLE", "BITPIX", "NAXIS", "NAXIS1", "NAXIS2", "NAXIS3", "EXTEND", "BZERO", "BSCALE",
    "END", "COMMENT", "HISTORY", "CONTINUE", "XTENSION", "PCOUNT", "GCOUNT",
];

fn card_from_kv(keyword: &str, value: &str) -> Option<Card> {
    let cv = if let Ok(i) = value.parse::<i64>() {
        CardValue::Integer(i)
    } else if let Ok(f) = value.parse::<f64>() {
        CardValue::Real(f)
    } else {
        CardValue::Str(value.to_string())
    };
    Card::new(keyword, cv).ok()
}

fn convert(src: &Path, dst: &Path) -> anyhow::Result<()> {
    let header = FitsHeader::from_path(src)?;
    let keys = parse_stored_header_keys(FileFormat::FITS, &header.to_header_text());
    let mut cards: Vec<Card> = keys
        .iter()
        .filter(|(k, _)| !STRUCTURAL.contains(&k.as_str()))
        .filter_map(|(k, v)| card_from_kv(k, v))
        .collect();
    cards.sort_by(|a, b| a.keyword.cmp(&b.keyword));

    let (meta, pixels) = astroimage::ImageConverter::read_raw(src)?;
    let data: Vec<f32> = match pixels {
        astroimage::PixelData::Uint16(v) => v.into_iter().map(f32::from).collect(),
        astroimage::PixelData::Float32(v) => v,
    };
    anyhow::ensure!(
        data.len() == meta.width * meta.height * meta.channels,
        "pixel count {} != {}x{}x{}",
        data.len(),
        meta.width,
        meta.height,
        meta.channels
    );
    write_image_f32(
        dst,
        meta.width,
        meta.height,
        meta.channels,
        &data,
        &cards,
        OutputFormat::Xisf,
        XisfBounds::Adu16,
    )?;
    println!(
        "{} -> {} ({}x{}x{}, {} cards)",
        src.display(),
        dst.display(),
        meta.width,
        meta.height,
        meta.channels,
        cards.len()
    );
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(
        !args.is_empty() && args.len() % 2 == 0,
        "usage: xisf_convert_probe <src.fits> <dst.xisf> [<src> <dst>…]"
    );
    for pair in args.chunks(2) {
        convert(Path::new(&pair[0]), Path::new(&pair[1]))?;
    }
    Ok(())
}

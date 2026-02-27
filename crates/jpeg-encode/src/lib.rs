use image::codecs::jpeg::JpegEncoder;
use image::{ColorType, ImageEncoder};
use std::io::Cursor;

/// Encode raw RGB pixels to JPEG in memory.
///
/// This crate exists outside the Cargo workspace so the generic JpegEncoder
/// monomorphization happens here (with full optimisation from
/// `[profile.dev.package."*"]`) rather than in a workspace crate where
/// incremental compilation splits codegen units and prevents inlining.
pub fn encode_rgb_to_jpeg(
    rgb_data: &[u8],
    width: u32,
    height: u32,
    quality: u8,
) -> Result<Vec<u8>, String> {
    let mut buf = Cursor::new(Vec::with_capacity((width * height) as usize));
    let encoder = JpegEncoder::new_with_quality(&mut buf, quality);
    encoder
        .write_image(rgb_data, width, height, ColorType::Rgb8.into())
        .map_err(|e| format!("JPEG encode failed: {}", e))?;
    Ok(buf.into_inner())
}

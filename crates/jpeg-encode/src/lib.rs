/// Encode raw RGB pixels to JPEG in memory.
///
/// Backed by `turbojpeg` (C libjpeg-turbo, SIMD DCT) — the same encoder the
/// rustafits submodule uses, so the workspace carries one JPEG codec and the
/// full-frame preview encode stays in libjpeg-turbo territory (~119 ms at
/// 6248x4176) instead of a scalar pure-Rust path (~891 ms). The parked
/// `wip/pure-rust-jpeg` branch swaps this backend for `libjpeg-turbo-rs`
/// once that crate's 0.8.0 (trailing-MCU fix) is published.
///
/// This crate stays outside the Cargo workspace so consumers keep a stable
/// seam to swap the backend without touching workspace profiles.
pub fn encode_rgb_to_jpeg(
    rgb_data: &[u8],
    width: u32,
    height: u32,
    quality: u8,
) -> Result<Vec<u8>, String> {
    let (w, h) = (width as usize, height as usize);
    let expected = w
        .checked_mul(h)
        .and_then(|n| n.checked_mul(3))
        .ok_or_else(|| "image dimensions overflow".to_string())?;
    if rgb_data.len() < expected {
        return Err(format!(
            "pixel buffer too small: {} bytes, need {expected} for {width}x{height}x3",
            rgb_data.len()
        ));
    }

    let image = turbojpeg::Image {
        pixels: rgb_data,
        width: w,
        pitch: w * 3,
        height: h,
        format: turbojpeg::PixelFormat::RGB,
    };
    turbojpeg::compress(image, quality as i32, turbojpeg::Subsamp::Sub2x2)
        .map(|jpeg| jpeg.to_vec())
        .map_err(|e| format!("JPEG encode failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_a_jpeg_stream() {
        let rgb: Vec<u8> = (0..32 * 24 * 3).map(|i| (i % 251) as u8).collect();
        let jpeg = encode_rgb_to_jpeg(&rgb, 32, 24, 85).unwrap();
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "SOI marker");
        assert_eq!(&jpeg[jpeg.len() - 2..], &[0xFF, 0xD9], "EOI marker");
    }

    #[test]
    fn rejects_short_buffer() {
        let rgb = [0u8; 10];
        assert!(encode_rgb_to_jpeg(&rgb, 32, 24, 85).is_err());
    }
}

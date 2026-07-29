//! In-memory JPEG encoding for preview/thumbnail generation.
//!
//! Backed by `libjpeg-turbo-rs`, a pure-Rust libjpeg-turbo reimplementation
//! (NEON on aarch64, AVX2/SSE2 on x86_64, scalar fallback elsewhere) — the same
//! encoder, at the same pinned version, that the rustafits submodule uses, so
//! the workspace carries one JPEG codec.
//!
//! This replaced the `turbojpeg` C binding at comparable speed and with no C
//! toolchain. That matters beyond convenience: this crate is an
//! **unconditional** dependency of `athenaeum-core`, so its native build
//! requirements were also the requirements of every headless consumer —
//! `cargo check -p athenaeum-core --no-default-features` and the arm64 Perseus
//! container included. Those now need nothing but the Rust toolchain.
//!
//! This crate stays outside the Cargo workspace (see the `exclude` in the root
//! `Cargo.toml`) purely as a stable seam for swapping the JPEG backend without
//! touching workspace profiles. The old performance rationale is dead: it
//! applied to the `image` crate's generic `JpegEncoder`, which had to be
//! monomorphized here under `[profile.dev.package."*"]`. `libjpeg_turbo_rs::compress`
//! is a non-generic registry-crate fn that `package."*"` already covers, and
//! what remains here is a ~40-line wrapper where opt-level is irrelevant.
//!
//! KNOWN COST: because this crate is excluded, `cargo test --workspace` does
//! NOT run the tests below. Run `cargo test` in `crates/jpeg-encode` when
//! touching this file.

/// Pixel layouts this encoder accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// 3 bytes per pixel.
    Rgb,
    /// 4 bytes per pixel; the alpha byte is read and discarded by the encoder,
    /// so callers must NOT de-interleave to RGB first.
    Rgba,
}

impl Layout {
    fn bytes_per_pixel(self) -> usize {
        match self {
            Layout::Rgb => 3,
            Layout::Rgba => 4,
        }
    }

    fn pixel_format(self) -> libjpeg_turbo_rs::PixelFormat {
        match self {
            Layout::Rgb => libjpeg_turbo_rs::PixelFormat::Rgb,
            Layout::Rgba => libjpeg_turbo_rs::PixelFormat::Rgba,
        }
    }
}

/// Encode raw pixels to a baseline JPEG (4:2:0 chroma subsampling) in memory.
///
/// Prefer this over [`encode_rgb_to_jpeg`] when the source buffer is RGBA:
/// passing [`Layout::Rgba`] lets the encoder drop alpha inline and saves a full
/// `width * height * 3` de-interleaving copy per frame.
///
/// `quality` is clamped to `1..=100`. The backend clamps internally too; doing
/// it here makes the documented range a property of this function rather than
/// of whichever encoder is underneath. Note this differs from the old
/// `turbojpeg` binding, which returned an error for an out-of-range quality.
pub fn encode_to_jpeg(
    data: &[u8],
    width: u32,
    height: u32,
    layout: Layout,
    quality: u8,
) -> Result<Vec<u8>, String> {
    let (w, h) = (width as usize, height as usize);
    let required = w
        .checked_mul(h)
        .and_then(|n| n.checked_mul(layout.bytes_per_pixel()))
        .ok_or_else(|| format!("JPEG encode failed: dimensions overflow ({width}x{height})"))?;

    if data.len() < required {
        return Err(format!(
            "JPEG encode failed: pixel buffer too small ({} bytes, need {required} for {width}x{height} {layout:?})",
            data.len()
        ));
    }

    // Output stability rests on four defaults this call does NOT name, because
    // they live inside `high_level::compress`: IsLow DCT, Annex-K quantization
    // tables scaled by quality, standard Annex-K Huffman tables (no two-pass
    // optimization), and no restart interval — the same choices C
    // libjpeg-turbo makes by default, which is why the switch off `turbojpeg`
    // was byte-stable. None of them is in the signature, so the `=0.8.0` pin in
    // Cargo.toml is what protects that; a version bump can change any silently.
    libjpeg_turbo_rs::compress(
        data,
        w,
        h,
        layout.pixel_format(),
        quality.clamp(1, 100),
        libjpeg_turbo_rs::Subsampling::S420,
    )
    .map_err(|e| format!("JPEG encode failed: {e}"))
}

/// Encode raw RGB pixels to JPEG in memory.
///
/// Kept for existing call sites; equivalent to
/// `encode_to_jpeg(.., Layout::Rgb, ..)`.
pub fn encode_rgb_to_jpeg(
    rgb_data: &[u8],
    width: u32,
    height: u32,
    quality: u8,
) -> Result<Vec<u8>, String> {
    encode_to_jpeg(rgb_data, width, height, Layout::Rgb, quality)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: u32, h: u32, bpp: usize) -> Vec<u8> {
        let mut v = Vec::with_capacity(w as usize * h as usize * bpp);
        for y in 0..h {
            for x in 0..w {
                let r = (x * 255 / w.max(1)) as u8;
                let g = (y * 255 / h.max(1)) as u8;
                v.extend_from_slice(&[r, g, 128]);
                if bpp == 4 {
                    v.push(255);
                }
            }
        }
        v
    }

    /// Round-trip guard: valid JPEG framing plus lossy-close pixels. Catches a
    /// wrong pixel format or channel order, which a compile check would not.
    fn round_trip(layout: Layout) {
        let (w, h) = (160u32, 96u32);
        let src = gradient(w, h, layout.bytes_per_pixel());

        let jpeg = encode_to_jpeg(&src, w, h, layout, 92).expect("encode failed");
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "missing SOI marker");
        assert_eq!(&jpeg[jpeg.len() - 2..], &[0xFF, 0xD9], "missing EOI marker");

        let decoded = libjpeg_turbo_rs::decompress_to(&jpeg, libjpeg_turbo_rs::PixelFormat::Rgb)
            .expect("decode failed");
        assert_eq!((decoded.width as u32, decoded.height as u32), (w, h));

        let bpp = layout.bytes_per_pixel();
        let mut sse = 0f64;
        for i in 0..(w as usize * h as usize) {
            for c in 0..3 {
                let a = src[i * bpp + c] as f64;
                let b = decoded.data[i * 3 + c] as f64;
                sse += (a - b) * (a - b);
            }
        }
        let mse = sse / (w as f64 * h as f64 * 3.0);
        let psnr = 10.0 * (255.0f64 * 255.0 / mse).log10();
        assert!(
            psnr > 35.0,
            "round-trip PSNR too low for {layout:?}: {psnr:.1} dB"
        );
    }

    #[test]
    fn round_trip_rgb() {
        round_trip(Layout::Rgb);
    }

    #[test]
    fn round_trip_rgba_ignores_alpha() {
        round_trip(Layout::Rgba);
    }

    /// RGBA in must produce the same bytes as the hand-stripped RGB the old
    /// call site built — proves dropping the de-interleave copy is a no-op on
    /// the output, not merely "close enough".
    #[test]
    fn rgba_matches_hand_stripped_rgb() {
        let (w, h) = (64u32, 48u32);
        let rgba = gradient(w, h, 4);
        let rgb: Vec<u8> = rgba
            .chunks_exact(4)
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect();

        let from_rgba = encode_to_jpeg(&rgba, w, h, Layout::Rgba, 85).unwrap();
        let from_rgb = encode_to_jpeg(&rgb, w, h, Layout::Rgb, 85).unwrap();
        assert_eq!(from_rgba, from_rgb);
    }

    /// The RGBA guard above only means something if the buffers really differ:
    /// a bug that read 3 bytes per pixel from the RGBA buffer would also pass
    /// it if alpha happened to be invisible. Encoding the RGBA bytes AS RGB
    /// must produce different output.
    #[test]
    fn rgba_layout_is_not_a_no_op() {
        let (w, h) = (64u32, 48u32);
        let rgba = gradient(w, h, 4);
        let as_rgba = encode_to_jpeg(&rgba, w, h, Layout::Rgba, 85).unwrap();
        // Same dimensions, but Rgb consumes only the first w*h*3 bytes of the
        // RGBA buffer, so every channel after the first pixel is misaligned —
        // a genuinely different image, hence a different stream.
        let as_rgb = encode_to_jpeg(&rgba, w, h, Layout::Rgb, 85).unwrap();
        assert_ne!(as_rgba, as_rgb);
    }

    #[test]
    fn rejects_short_buffer() {
        let err = encode_rgb_to_jpeg(&[0u8; 8], 32, 32, 90).unwrap_err();
        assert!(err.contains("too small"), "got: {err}");
    }
}

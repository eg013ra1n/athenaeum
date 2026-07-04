//! Flat-frame contour analysis — port of PixInsight's FlatContourPlot v1.3.1
//! script (Mike Schuster, 2012-2021), translated to Rust.
//!
//! Pipeline (faithful to the PJSR script — see the user's reference):
//!   1. Read raw pixels, coerce to single-channel f32.
//!   2. Resample to `resolution_pct%` of the original dimensions (block-mean).
//!   3. Gather sorted samples; take `minQ = sample[0.01 N]` and
//!      `maxQ = sample[0.99 N]`. Refuse if image is too small or too uniform
//!      (PI bails at < 128×128 or `maxQ == minQ`).
//!   4. Rescale: `pixel = clamp((pixel - minQ) / (maxQ - minQ), 0, 1)` —
//!      maps the percentile-clipped flat to the full `[0, 1]` working range.
//!   5. Gaussian blur with `sigma_px` (separable, *normalized* kernel,
//!      mirror borders). Implemented locally — `astroimage`'s
//!      `generate_1d_kernel` is a zero-sum derivative kernel and would
//!      destroy the DC component of a flat.
//!   6. Quantize:
//!        a. `q = floor(contours * pixel + 0.5) / contours` → discrete
//!           levels `0, 1/c, 2/c, …, 1`.
//!        b. Map `[0, 1]` → `[minTone, maxTone] = [0.05, 0.95]` so the bands
//!           don't go pure black or pure white.
//!   7. Compute the gradient magnitude of the *quantized* image using PI's
//!      two 3×3 derivative-of-Gaussian (σ = 0.5) kernels, then rescale to
//!      `[0, 1]`.
//!   8. Boundary emphasis:
//!        `final = clamp(quantized * (1 - 0.01 * gradient_pct * gradient), 0, 1)`
//!      — darkens the band boundaries (where the gradient of the discrete
//!      quantized image is high). This is what creates the visible contour
//!      lines in the output.
//!   9. Convert to 8-bit grayscale.
//!
//! The frontend renders the byte buffer directly via `putImageData` and
//! draws the legend strip with `contours + 1` boundary values labelled as
//! `minQ + (maxQ - minQ) * (i / contours)` for `i ∈ [0, contours]` —
//! i.e., in the **original flat units**, not the rescaled `[0, 1]` space.

use anyhow::{anyhow, Context, Result};
use astroimage::{ImageConverter, ImageMetadata, PixelData};

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct FlatContourOpts {
    /// Resampling factor in percent. Final width/height are
    /// `original * resolution_pct / 100`. PI range: 5..=100.
    pub resolution_pct: u32,
    /// Gaussian blur sigma in pixels (post-resample). PI range: 0..=10.
    pub sigma_px: f32,
    /// Number of discrete contour bands. PI range: 4..=20 (default 15).
    pub contours: u32,
    /// Boundary-emphasis strength as a percentage. PI range: 0..=200,
    /// default 50. Higher values darken the band boundaries more.
    pub gradient_pct: u32,
}

impl Default for FlatContourOpts {
    fn default() -> Self {
        Self {
            resolution_pct: 50,
            sigma_px: 1.0,
            contours: 15,
            gradient_pct: 50,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlatContourResult {
    /// Resampled width.
    pub width: u32,
    /// Resampled height.
    pub height: u32,
    /// Raw-units peak of the resampled, pre-rescale image.
    pub peak: f32,
    /// Raw-units mean.
    pub mean: f32,
    /// Raw-units min.
    pub min: f32,
    /// 1st-percentile sample value (legend low edge, original flat units).
    pub min_quantile: f32,
    /// 99th-percentile sample value (legend high edge, original flat units).
    pub max_quantile: f32,
    /// Echoed back so the frontend can build the legend without recomputing.
    pub contours: u32,
    /// Final 8-bit grayscale display image. Length == `width * height`.
    pub pixels: Vec<u8>,
}

const MIN_TONE: f32 = 0.05;
const MAX_TONE: f32 = 0.95;
const MIN_DIMENSION: usize = 128;

pub fn compute_flat_contour_plot(
    path: &str,
    opts: FlatContourOpts,
) -> Result<FlatContourResult> {
    // ── 1. read ─────────────────────────────────────────────────────────
    let (meta, pixels) = ImageConverter::read_raw(path)
        .with_context(|| format!("read_raw({path})"))?;

    // ── 2. single-channel f32 ───────────────────────────────────────────
    let mut mono = to_mono_f32(&meta, &pixels);
    let src_w = meta.width;
    let src_h = meta.height;

    // Apply astronomical orientation. We use the centralized helper
    // (`crate::orientation::flip_vertical_for_path`) instead of the
    // `meta.flip_vertical` returned by rustafits so a single source of
    // truth governs every display surface. The helper reads ROWORDER
    // directly and applies the corrected rule (flip for BOTTOM-UP /
    // missing, don't flip for TOP-DOWN — see `orientation` module).
    if src_h > 1 && crate::orientation::flip_vertical_for_path(std::path::Path::new(path)) {
        flip_vertical_in_place(&mut mono, src_w, src_h);
    }

    // ── 3. resample ─────────────────────────────────────────────────────
    let pct = opts.resolution_pct.clamp(5, 100) as usize;
    let dst_w = (src_w * pct / 100).max(1);
    let dst_h = (src_h * pct / 100).max(1);
    let mut buf = block_mean_downsample(&mono, src_w, src_h, dst_w, dst_h);

    // ── 4. raw stats (pre-rescale, post-resample) ───────────────────────
    let (peak, mean, min) = compute_stats(&buf);

    // ── 5. percentile bounds ────────────────────────────────────────────
    if dst_w < MIN_DIMENSION || dst_h < MIN_DIMENSION {
        return Err(anyhow!(
            "Resampled image is too small ({}×{}). Increase Resolution.",
            dst_w, dst_h
        ));
    }
    let (min_q, max_q) = percentile_bounds(&buf, 0.01, 0.99);
    if max_q <= min_q {
        return Err(anyhow!(
            "Image has insufficient dynamic range (1st/99th percentiles equal at {}).",
            min_q
        ));
    }

    // ── 6. rescale to [0, 1] ────────────────────────────────────────────
    let span = max_q - min_q;
    for v in buf.iter_mut() {
        *v = ((*v - min_q) / span).clamp(0.0, 1.0);
    }

    // ── 7. Gaussian blur (normalized, separable, mirror borders) ───────
    if opts.sigma_px > 0.0 {
        gaussian_blur_inplace(&mut buf, dst_w, dst_h, opts.sigma_px.min(10.0));
    }

    // ── 8. quantize → [minTone, maxTone] ────────────────────────────────
    let n_bands = opts.contours.clamp(4, 20) as f32;
    let tone_span = MAX_TONE - MIN_TONE;
    let mut quantized = vec![0.0_f32; buf.len()];
    for (q, &v) in quantized.iter_mut().zip(buf.iter()) {
        let level = (n_bands * v + 0.5).floor() / n_bands; // {0, 1/c, 2/c, …, 1}
        *q = (level * tone_span + MIN_TONE).clamp(0.0, 1.0);
    }

    // ── 9. gradient magnitude of the quantized image ────────────────────
    let mut gradient = compute_gradient_magnitude(&quantized, dst_w, dst_h);
    rescale_to_unit(&mut gradient);

    // ── 10. boundary-emphasis blend → 8-bit ─────────────────────────────
    let g_strength = 0.01 * opts.gradient_pct.clamp(0, 200) as f32;
    let mut out_pixels = vec![0_u8; buf.len()];
    for ((dst, &q), &g) in out_pixels.iter_mut().zip(quantized.iter()).zip(gradient.iter()) {
        let mut v = q * (1.0 - g_strength * g);
        if v < 0.0 { v = 0.0; }
        if v > 1.0 { v = 1.0; }
        *dst = (v * 255.0 + 0.5) as u8;
    }

    Ok(FlatContourResult {
        width: dst_w as u32,
        height: dst_h as u32,
        peak,
        mean,
        min,
        min_quantile: min_q,
        max_quantile: max_q,
        contours: opts.contours,
        pixels: out_pixels,
    })
}

// ───────────────────────────────────────────────────────────────────────────
// helpers
// ───────────────────────────────────────────────────────────────────────────

/// Pull a single luminance channel out of the raw FITS pixels. RGB takes
/// the green plane (PI's default for flat analysis).
fn to_mono_f32(meta: &ImageMetadata, pixels: &PixelData) -> Vec<f32> {
    let n = meta.width * meta.height;
    let channels = meta.channels.max(1);
    let plane_offset = if channels >= 3 { n } else { 0 }; // green plane
    match pixels {
        PixelData::Uint16(data) => {
            let src = &data[plane_offset..plane_offset + n];
            src.iter().map(|&v| v as f32).collect()
        }
        PixelData::Float32(data) => {
            let src = &data[plane_offset..plane_offset + n];
            src.to_vec()
        }
    }
}

/// Flip the image rows in place so row 0 ↔ row h-1, row 1 ↔ row h-2, etc.
/// Used to apply rustafits' `flip_vertical` metadata flag (FITS rows are
/// stored bottom-to-top by convention; display expects top-to-bottom).
fn flip_vertical_in_place(buf: &mut [f32], w: usize, h: usize) {
    if h < 2 || buf.len() != w * h {
        return;
    }
    for y in 0..(h / 2) {
        let top_start = y * w;
        let bot_start = (h - 1 - y) * w;
        // Two slices that don't overlap.
        let (lo, hi) = buf.split_at_mut(bot_start);
        let top = &mut lo[top_start..top_start + w];
        let bot = &mut hi[..w];
        top.swap_with_slice(bot);
    }
}

/// Block-mean downsample from (sw,sh) to (dw,dh). Each destination pixel
/// is the mean of the rectangular block of source pixels that maps to it.
fn block_mean_downsample(
    src: &[f32],
    sw: usize,
    sh: usize,
    dw: usize,
    dh: usize,
) -> Vec<f32> {
    if dw == sw && dh == sh {
        return src.to_vec();
    }
    let mut out = vec![0.0_f32; dw * dh];
    for dy in 0..dh {
        let y0 = dy * sh / dh;
        let y1 = ((dy + 1) * sh / dh).max(y0 + 1).min(sh);
        for dx in 0..dw {
            let x0 = dx * sw / dw;
            let x1 = ((dx + 1) * sw / dw).max(x0 + 1).min(sw);
            let mut sum = 0.0_f64;
            let mut count = 0_u32;
            for y in y0..y1 {
                let row = y * sw;
                for x in x0..x1 {
                    sum += src[row + x] as f64;
                    count += 1;
                }
            }
            out[dy * dw + dx] = if count > 0 {
                (sum / count as f64) as f32
            } else {
                0.0
            };
        }
    }
    out
}

fn compute_stats(buf: &[f32]) -> (f32, f32, f32) {
    if buf.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    let mut peak = f32::NEG_INFINITY;
    let mut min = f32::INFINITY;
    let mut sum = 0.0_f64;
    for &v in buf {
        if v.is_finite() {
            if v > peak { peak = v; }
            if v < min { min = v; }
            sum += v as f64;
        }
    }
    let mean = (sum / buf.len() as f64) as f32;
    if !peak.is_finite() { peak = 0.0; }
    if !min.is_finite() { min = 0.0; }
    (peak, mean, min)
}

/// Returns `(p1, p99)` percentile values via partial sort.
fn percentile_bounds(buf: &[f32], lo_q: f32, hi_q: f32) -> (f32, f32) {
    if buf.is_empty() {
        return (0.0, 1.0);
    }
    let mut copy: Vec<f32> = buf.iter().copied().filter(|v| v.is_finite()).collect();
    if copy.len() < 2 {
        return (0.0, 1.0);
    }
    let n = copy.len();
    let lo_idx = ((lo_q * n as f32) as usize).min(n - 1);
    let hi_idx = ((hi_q * n as f32) as usize).min(n - 1);
    let cmp = |a: &f32, b: &f32| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal);
    copy.select_nth_unstable_by(lo_idx, cmp);
    let lo = copy[lo_idx];
    copy.select_nth_unstable_by(hi_idx, cmp);
    let hi = copy[hi_idx];
    (lo, hi)
}

/// Build a length-`(2r+1)` Gaussian kernel that sums to 1.
fn gaussian_kernel_normalized(sigma: f32) -> Vec<f32> {
    let sigma = sigma.max(1e-3);
    let radius = ((3.0 * sigma).ceil() as usize).max(1);
    let ksize = 2 * radius + 1;
    let inv_2s2 = 1.0 / (2.0 * sigma * sigma);
    let mut kernel = Vec::with_capacity(ksize);
    let mut sum = 0.0_f32;
    for i in 0..ksize {
        let d = i as f32 - radius as f32;
        let g = (-inv_2s2 * d * d).exp();
        kernel.push(g);
        sum += g;
    }
    for v in kernel.iter_mut() {
        *v /= sum;
    }
    kernel
}

/// Reflect-at-border index. Out-of-range indices mirror back into `[0, n)`.
#[inline]
fn mirror(i: i32, n: i32) -> usize {
    if n <= 0 { return 0; }
    let mut x = i;
    if x < 0 { x = -x - 1; }
    if x >= n { x = 2 * n - x - 1; }
    if x < 0 { x = 0; }
    if x >= n { x = n - 1; }
    x as usize
}

/// Separable Gaussian blur with mirror borders, in-place.
fn gaussian_blur_inplace(buf: &mut [f32], w: usize, h: usize, sigma: f32) {
    let kernel = gaussian_kernel_normalized(sigma);
    let radius = (kernel.len() / 2) as i32;
    let mut tmp = vec![0.0_f32; w * h];
    let wi = w as i32;
    let hi = h as i32;

    // Horizontal pass.
    for y in 0..h {
        let row = y * w;
        for x in 0..w {
            let xi = x as i32;
            let mut acc = 0.0_f32;
            for k in -radius..=radius {
                let xx = mirror(xi + k, wi);
                acc += buf[row + xx] * kernel[(k + radius) as usize];
            }
            tmp[row + x] = acc;
        }
    }
    // Vertical pass.
    for y in 0..h {
        let yi = y as i32;
        for x in 0..w {
            let mut acc = 0.0_f32;
            for k in -radius..=radius {
                let yy = mirror(yi + k, hi);
                acc += tmp[yy * w + x] * kernel[(k + radius) as usize];
            }
            buf[y * w + x] = acc;
        }
    }
}

/// Compute the gradient-magnitude image using PI's two 3×3 derivative-of-
/// Gaussian (σ = 0.5) kernels with mirror borders. Output length matches
/// input length.
fn compute_gradient_magnitude(src: &[f32], w: usize, h: usize) -> Vec<f32> {
    // Derivative-of-Gaussian (sigma = 0.5) — copied verbatim from PI's
    // FlatContourPlot script. They sum to zero (zero DC) so a uniform
    // region has gradient = 0.
    const KX: [f32; 9] = [
         0.0496902,  0.40062,   0.0496902,
         0.0,        0.0,       0.0,
        -0.0496902, -0.40062,  -0.0496902,
    ];
    const KY: [f32; 9] = [
         0.0496902, 0.0, -0.0496902,
         0.40062,   0.0, -0.40062,
         0.0496902, 0.0, -0.0496902,
    ];

    let mut out = vec![0.0_f32; src.len()];
    let wi = w as i32;
    let hi = h as i32;
    for y in 0..h {
        let yi = y as i32;
        for x in 0..w {
            let xi = x as i32;
            let mut gx = 0.0_f32;
            let mut gy = 0.0_f32;
            for ky in -1..=1_i32 {
                for kx in -1..=1_i32 {
                    let xx = mirror(xi + kx, wi);
                    let yy = mirror(yi + ky, hi);
                    let p = src[yy * w + xx];
                    let kidx = ((ky + 1) * 3 + (kx + 1)) as usize;
                    gx += p * KX[kidx];
                    gy += p * KY[kidx];
                }
            }
            out[y * w + x] = (gx * gx + gy * gy).sqrt();
        }
    }
    out
}

/// Rescale a buffer to [0, 1] based on min/max. Constant buffers become 0.
fn rescale_to_unit(buf: &mut [f32]) {
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    for &v in buf.iter() {
        if v.is_finite() {
            if v < lo { lo = v; }
            if v > hi { hi = v; }
        }
    }
    if !lo.is_finite() || !hi.is_finite() || hi <= lo {
        for v in buf.iter_mut() {
            *v = 0.0;
        }
        return;
    }
    let span = hi - lo;
    for v in buf.iter_mut() {
        *v = ((*v - lo) / span).clamp(0.0, 1.0);
    }
}

use anyhow::{Context, Result};
use astroimage::ImageConverter;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

/// Serializes full-resolution gradient debayers.
///
/// One such render allocates twelve bytes per pixel of planar RGB — measured at
/// 547 MB peak RSS on a 6248x4176 one-shot-colour frame — and already saturates
/// the machine on its own (5.84 s of CPU inside 0.79 s of wall time). Running
/// several at once therefore multiplies the memory peak without buying
/// throughput: on that frame, five in parallel took 3.40 s against 3.99 s one
/// after another, for five times the peak.
///
/// Held by the HOST, before its image semaphore, never inside the render
/// (review 2026-09-06 F4): both hosts bound concurrent renders with one
/// `image_semaphore` shared by every thumbnail, preview and full request. A
/// gate taken after the permit parked N-1 permits here during a
/// full-resolution colour prefetch and starved every unrelated request behind
/// the whole VNG backlog. Lock order is gate → permit everywhere; a permit
/// holder never waits for the gate, so there is no cycle. Async so a waiting
/// request parks its task, not a runtime thread. [`needs_vng_gate`] says
/// whether a given request must take it.
pub static VNG_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Whether rendering `path` at `resolution` is a full-resolution gradient
/// debayer and must hold [`VNG_GATE`]. Header-only — a FITS primary header
/// or an XISF XML header, never the pixels — through the same readers the
/// scanner uses, so the answer is what the catalog would say. Errs towards
/// `true` when the header cannot be read: the render will fail loudly on its
/// own, and taking the gate for a broken file costs one serialization, not
/// gigabytes.
pub fn needs_vng_gate(path: &Path, resolution: Resolution) -> bool {
    if resolution != Resolution::Full {
        return false;
    }
    let is_xisf = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("xisf"));
    let bayerpat = if is_xisf {
        crate::fits_parser::parse_xisf(path, 0).map(|f| f.bayerpat)
    } else {
        crate::fits_parser::parse_fits(path, 0).map(|f| f.bayerpat)
    };
    match bayerpat {
        Ok(pat) => pat.is_some_and(|p| !p.trim().is_empty()),
        Err(error) => {
            tracing::warn!(path = %path.display(), error = %error, "header probe failed — taking the VNG gate defensively");
            true
        }
    }
}

/// Resolution variants for blink viewer
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Resolution {
    Thumbnail,
    Preview,
    Full,
}

impl Resolution {
    /// Parse resolution from string
    pub fn from_string(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "thumbnail" => Resolution::Thumbnail,
            "preview" => Resolution::Preview,
            "full" => Resolution::Full,
            _ => Resolution::Preview, // Default to preview
        }
    }

    /// Get JPEG quality for this resolution
    /// Uses custom quality if provided, otherwise defaults
    pub fn jpeg_quality(&self, custom: Option<u8>) -> u8 {
        if let Some(q) = custom {
            return q.clamp(1, 100);
        }
        match self {
            Resolution::Thumbnail => 70,
            Resolution::Preview => 85,
            Resolution::Full => 95,
        }
    }

    /// Get setting key for this resolution's quality
    pub fn quality_setting_key(&self) -> &'static str {
        match self {
            Resolution::Thumbnail => "rustafits.quality.thumbnail",
            Resolution::Preview => "rustafits.quality.preview",
            Resolution::Full => "rustafits.quality.full",
        }
    }

    /// Get downscale factor for this resolution
    pub fn downscale_factor(&self) -> usize {
        match self {
            Resolution::Thumbnail => 4,
            Resolution::Preview => 1, // preview mode uses 2x2 binning internally
            Resolution::Full => 1,
        }
    }

    /// Whether to use rustafits preview mode (2x2 binning)
    pub fn use_preview_mode(&self) -> bool {
        matches!(self, Resolution::Preview)
    }
}

/// Processed FITS image data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessedImage {
    pub image_data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub format: String, // "jpeg"
    pub is_color: bool, // Whether this is a color/Bayer image
}

/// Process a FITS/XISF file to JPEG entirely in memory (no temp files).
///
/// Uses `converter.process()` to get raw RGB pixels, then encodes JPEG
/// in-process via rustafits's own `encode_jpeg` (pure-Rust libjpeg-turbo-rs),
/// returning a `Vec<u8>`.
///
/// At [`Resolution::Full`] a CFA frame is debayered at its native resolution
/// with the gradient method instead of the super-pixel one, so "full
/// resolution" means what it says for one-shot-colour data. The host
/// serializes that render by holding [`VNG_GATE`] around this call when
/// [`needs_vng_gate`] says so; this function takes no lock itself.
///
/// Note: rustafits 0.2+ handles Bayer/color detection internally for both FITS and XISF
pub fn process_fits_to_jpeg<P: AsRef<Path>>(
    input_path: P,
    resolution: Resolution,
    quality: Option<u8>,
    pool: &Arc<rayon::ThreadPool>,
) -> Result<ProcessedImage> {
    let input_path = input_path.as_ref();

    // Validate input file exists
    if !input_path.exists() {
        anyhow::bail!("Input file does not exist: {}", input_path.display());
    }

    // Build rustafits converter with resolution-specific settings
    let mut converter = ImageConverter::new().with_thread_pool(pool.clone());

    // Apply downscale if needed (for thumbnails)
    if resolution.downscale_factor() > 1 {
        converter = converter.with_downscale(resolution.downscale_factor());
    }

    // Apply preview mode for faster processing
    if resolution.use_preview_mode() {
        converter = converter.with_preview_mode();
    }

    // Full resolution means native resolution, including for CFA data. Without
    // this the super-pixel debayer folds every 2x2 Bayer tile into one pixel
    // and hands back half of each axis under the name "full".
    if resolution == Resolution::Full {
        converter = converter.with_vng_debayer();
    }

    // Read and process are split so the gate below can be taken *only* for the
    // renders that need it — a mono full-resolution render is cheap and must
    // not queue behind one. `ImageConverter::read_raw` is an associated
    // function and, unlike `process`/`process_data`, does not install the
    // converter's thread pool, while the FITS reader parallelises its byte swap
    // with rayon. Installing the pool around it by hand is what keeps that work
    // on the app's bounded image pool instead of leaking onto the global one.
    let (meta, pixels) = pool
        .install(|| ImageConverter::read_raw(input_path))
        .with_context(|| format!("Failed to read image: {}", input_path.display()))?;

    // Process FITS/XISF to raw RGB pixels in memory
    let processed = converter
        .process_data(meta, pixels)
        .with_context(|| format!("Failed to process image: {}", input_path.display()))?;

    let width = processed.width as u32;
    let height = processed.height as u32;
    let is_color = processed.is_color;

    // Encode in memory via rustafits's own encoder. It accepts 3 (RGB) or
    // 4 (RGBA) channels and rejects anything else; for RGBA the encoder reads
    // 4 bytes per pixel and drops alpha itself, so no de-interleaving copy. In
    // practice this is always RGB: nothing in Athenaeum calls rustafits's
    // `with_rgba_output`, and the converter defaults to 3 channels.
    let jpeg_quality = resolution.jpeg_quality(quality);
    let image_data = astroimage::encode_jpeg(
        &processed.data,
        processed.width,
        processed.height,
        processed.channels as usize,
        jpeg_quality,
    )?;

    Ok(ProcessedImage {
        image_data,
        width,
        height,
        format: "jpeg".to_string(),
        is_color,
    })
}

/// User-configurable annotation display settings.
/// Stored as JSON in the settings table under key "blink.annotation_config".
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct AnnotationSettings {
    /// Color scheme: "eccentricity", "fwhm", or "uniform"
    pub color_scheme: String,
    /// Draw a direction tick along the elongation axis
    pub show_direction_tick: bool,
    /// Ellipse semi-axis scale in units of FWHM (semi-major = fwhm_x × scale).
    /// The historic hardcoded 2.5 drew ~50px lassos on oversampled frames,
    /// making clean single stars read as blends. Absent in stored JSON from
    /// older versions → serde default.
    #[serde(default = "default_ellipse_scale")]
    pub ellipse_scale: f32,
    /// Minimum ellipse semi-axis radius in output pixels
    pub min_radius: f32,
    /// Maximum ellipse semi-axis radius in output pixels
    pub max_radius: f32,
    /// Line thickness: 1 = single pixel, 2 = 3px cross, 3 = 5px diamond
    pub line_width: u8,
    /// Eccentricity threshold: below this is green (good)
    pub ecc_good: f32,
    /// Eccentricity threshold: above this is red (problem)
    pub ecc_warn: f32,
    /// FWHM ratio threshold: below this is green (good)
    pub fwhm_good: f32,
    /// FWHM ratio threshold: above this is red (problem)
    pub fwhm_warn: f32,
}

fn default_ellipse_scale() -> f32 {
    1.2
}

impl Default for AnnotationSettings {
    fn default() -> Self {
        Self {
            color_scheme: "eccentricity".to_string(),
            show_direction_tick: true,
            ellipse_scale: default_ellipse_scale(),
            min_radius: 6.0,
            max_radius: 60.0,
            line_width: 2,
            ecc_good: 0.5,
            ecc_warn: 0.6,
            fwhm_good: 1.3,
            fwhm_warn: 2.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::{write_fits_f32, Card, CardValue};

    fn image_pool() -> Arc<rayon::ThreadPool> {
        Arc::new(rayon::ThreadPoolBuilder::new().num_threads(2).build().unwrap())
    }

    /// A small CFA frame with a real Bayer pattern in the header, written
    /// through the app's own FITS writer so the reader sees exactly the shape
    /// it sees in production.
    fn write_cfa_fits(path: &std::path::Path, w: usize, h: usize) {
        let data: Vec<f32> = (0..w * h).map(|i| ((i * 37) % 4096) as f32).collect();
        let cards = vec![
            Card::new("BAYERPAT", CardValue::Str("RGGB".into())).unwrap(),
            Card::new("ROWORDER", CardValue::Str("TOP-DOWN".into())).unwrap(),
        ];
        write_fits_f32(path, w, h, 1, &data, &cards).unwrap();
    }

    /// The whole point of the feature: "full resolution" has to mean native
    /// resolution for one-shot-colour data too, while preview keeps the cheap
    /// half-size super-pixel debayer.
    #[test]
    fn full_resolution_debayers_cfa_at_native_size() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("cfa.fits");
        let (w, h) = (32, 32);
        write_cfa_fits(&path, w, h);
        let pool = image_pool();

        let full = process_fits_to_jpeg(&path, Resolution::Full, None, &pool).unwrap();
        assert_eq!((full.width as usize, full.height as usize), (w, h));
        assert!(full.is_color);

        let preview = process_fits_to_jpeg(&path, Resolution::Preview, None, &pool).unwrap();
        assert_eq!(
            (preview.width as usize, preview.height as usize),
            (w / 2, h / 2),
            "preview must keep the cheap super-pixel debayer"
        );
    }

    /// A mono frame is unaffected by the change and, being cheap, must not be
    /// serialized behind a colour render — this pins that `Full` on mono still
    /// produces a native-size grayscale image.
    #[test]
    fn full_resolution_leaves_mono_alone() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("mono.fits");
        let (w, h) = (32, 32);
        let data: Vec<f32> = (0..w * h).map(|i| ((i * 11) % 4096) as f32).collect();
        write_fits_f32(
            &path,
            w,
            h,
            1,
            &data,
            &[Card::new("ROWORDER", CardValue::Str("TOP-DOWN".into())).unwrap()],
        )
        .unwrap();

        let out = process_fits_to_jpeg(&path, Resolution::Full, None, &image_pool()).unwrap();
        assert_eq!((out.width as usize, out.height as usize), (w, h));
        assert!(!out.is_color);
    }

    /// Review 2026-09-06 F4: the gate moved out of the render and in front of
    /// the host's semaphore; the host decides from the header alone. Only a
    /// full-resolution render of a CFA frame needs it.
    #[test]
    fn needs_vng_gate_only_for_cfa_at_full() {
        let dir = tempfile::TempDir::new().unwrap();
        let cfa = dir.path().join("cfa.fits");
        write_cfa_fits(&cfa, 16, 16);
        let mono = dir.path().join("mono.fits");
        write_fits_f32(&mono, 16, 16, 1, &vec![1.0f32; 256], &[]).unwrap();

        assert!(needs_vng_gate(&cfa, Resolution::Full), "CFA at Full is the VNG render");
        assert!(!needs_vng_gate(&cfa, Resolution::Preview), "preview keeps the super-pixel path");
        assert!(!needs_vng_gate(&cfa, Resolution::Thumbnail));
        assert!(!needs_vng_gate(&mono, Resolution::Full), "mono must never queue behind a colour render");
        assert!(
            needs_vng_gate(&dir.path().join("missing.fits"), Resolution::Full),
            "an unreadable header errs towards taking the gate"
        );
    }
}

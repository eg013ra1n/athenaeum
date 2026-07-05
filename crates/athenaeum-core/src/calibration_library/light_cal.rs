//! Band-streaming light-calibration engine (B5, design spec
//! 2026-07-05-light-calibration-design.md §2). Applies a master dark (or bias)
//! and a master flat to one LIGHT frame, producing a calibrated 32-bit-float
//! FITS with negatives preserved (no clamping, no pedestal).
//!
//! Math (spec §2, verbatim):
//! ```text
//! L_c = ((L − S) / (F / divisor)) / OUTPUT_SCALE_DIVISOR
//! ```
//! where `S` = master dark if linked, else master bias, else no subtraction;
//! `F` = master flat when linked (division skipped otherwise); `divisor` =
//! the flat-normalization constant when normalization is on, else `1.0`; and
//! `OUTPUT_SCALE_DIVISOR = 65535.0` normalizes 16-bit sources to ~[0,1].
//!
//! Memory is bounded: the source frames are streamed one row-band at a time
//! (same budget policy as the master-integration engine), and only the single
//! output plane is held in RAM — mirroring what a master build already does
//! for its output. Geometry is validated by [`BandSource::open`], which
//! rejects mixed dimensions, so a size mismatch surfaces as
//! [`IntegrationError::BadInput`].
//!
//! This module owns only the pixel math + file write. The header card list is
//! built by [`crate::calibration_library::light_headers`] (Task 5) and handed
//! in via [`LightCalInputs::cards`]; the output path is resolved by the
//! orchestration layer.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::duplicates::compute_xxhash;
use crate::fits_parser::parse_fits_with_header;
use crate::fits_parser::stored_header::parse_stored_header_keys;
use crate::fits_writer::{write_fits_f32, Card};
use crate::integration::banded::{band_rows_for_budget, BandSource};
use crate::integration::engine::{central_third_mean, BAND_BUDGET_BYTES};
use crate::integration::IntegrationError;
use crate::models::FileFormat;

/// Bump when the calibration math changes — every existing tracking row then
/// derives as stale (see [`crate::db::light_calibrations::derive_status`],
/// which re-exports this constant). Single definition lives here, alongside
/// the engine whose behavior it versions.
pub const LIGHT_CAL_ENGINE_VERSION: i64 = 1;

/// Divisor that normalizes a 16-bit source's counts to roughly `[0, 1]`
/// (spec §2; stamped into the output header as `ATH_CSCL`). Kept as a single
/// constant so the scale decision lives in exactly one place.
pub const OUTPUT_SCALE_DIVISOR: f64 = 65535.0;

/// Everything the engine needs to calibrate one LIGHT frame.
///
/// `dark_path`/`bias_path` are the master subtrahend candidates — a dark is
/// preferred when both are present (raw-master-dark convention removes bias
/// and dark in one subtraction). `flat_path` is the illumination master.
/// `cards` is the fully-built output header (from Task 5's header builder);
/// the engine writes it through verbatim and makes no assumptions about it.
pub struct LightCalInputs {
    pub light_path: PathBuf,
    pub dark_path: Option<PathBuf>,
    pub bias_path: Option<PathBuf>,
    pub flat_path: Option<PathBuf>,
    /// Normalize the master flat by its `ATH_FNRM` constant (spec §2 toggle,
    /// default on). `false` → plain division by the flat as stored.
    pub flat_norm: bool,
    pub output_path: PathBuf,
    pub cards: Vec<Card>,
    pub scratch_dir: PathBuf,
}

/// What the engine actually applied, for the tracking row + progress summary.
#[derive(Debug, Clone, PartialEq)]
pub struct LightCalOutcome {
    /// Applied-state flags (`"BDF"`, `"BF"`, `"BD"`, `"B"`, `"F"`).
    pub calstat: String,
    /// Divisor actually used for flat normalization: the `ATH_FNRM` value when
    /// normalization was on, else `1.0` (also `1.0` when no flat was applied).
    pub flat_norm_divisor: f64,
    /// xxh3 of the written output file.
    pub output_hash: String,
}

/// Calibrate one LIGHT frame and write the result to `inputs.output_path`.
///
/// Cancellation is cooperative: checked once per band before any pixel work,
/// so a cancel that lands before the write leaves no output file behind.
pub fn calibrate_light(
    inputs: &LightCalInputs,
    cancel: &AtomicBool,
) -> Result<LightCalOutcome, IntegrationError> {
    // Subtrahend: dark preferred, else bias (spec §2 fallback order). The
    // calstat prefix records what was actually subtracted.
    let (subtrahend, calstat_base): (Option<&PathBuf>, &str) =
        match (&inputs.dark_path, &inputs.bias_path) {
            (Some(dark), _) => (Some(dark), "BD"),
            (None, Some(bias)) => (Some(bias), "B"),
            (None, None) => (None, ""),
        };
    let has_flat = inputs.flat_path.is_some();
    if subtrahend.is_none() && !has_flat {
        // Orchestration (Task 5) never calibrates with nothing linked, but the
        // engine refuses a meaningless raw-scale copy rather than write one.
        return Err(IntegrationError::BadInput(
            "no calibration frames linked (no dark, bias, or flat)".into(),
        ));
    }

    // Flat-normalization divisor actually applied: ATH_FNRM when on, else 1.0
    // (also 1.0 when no flat). Resolved before the read so a missing flat or a
    // bad ATH_FNRM fails fast, before any output work.
    let flat_norm_divisor = match (&inputs.flat_path, inputs.flat_norm) {
        (Some(flat), true) => flat_norm_constant(flat, &inputs.scratch_dir)?,
        _ => 1.0,
    };

    // One BandSource over light + subtrahend? + flat?, remembering each frame's
    // index. BandSource::open validates geometry itself and rejects mixed
    // dimensions as IntegrationError::BadInput — no separate pre-check needed.
    let mut paths: Vec<PathBuf> = vec![inputs.light_path.clone()];
    let sub_idx = subtrahend.map(|p| {
        paths.push(p.clone());
        paths.len() - 1
    });
    let flat_idx = inputs.flat_path.as_ref().map(|p| {
        paths.push(p.clone());
        paths.len() - 1
    });

    let mut src = BandSource::open(&paths, &inputs.scratch_dir)?;
    let (w, h, n) = (src.width(), src.height(), src.frame_count());
    let band_rows = band_rows_for_budget(w, n, BAND_BUDGET_BYTES).min(h);
    let mut band_bufs: Vec<Vec<f32>> = vec![Vec::new(); n];
    let mut out = vec![0f32; w * h];

    let mut y = 0;
    while y < h {
        if cancel.load(Ordering::Relaxed) {
            return Err(IntegrationError::Cancelled);
        }
        let rows = band_rows.min(h - y);
        src.read_band(y, rows, &mut band_bufs)?;
        let out_band = &mut out[y * w..(y + rows) * w];
        for (idx, out_px) in out_band.iter_mut().enumerate() {
            // f64 throughout, cast once at the end — negatives and division are
            // preserved with no clamping or pedestal (spec §2).
            let mut v = band_bufs[0][idx] as f64;
            if let Some(si) = sub_idx {
                v -= band_bufs[si][idx] as f64;
            }
            if let Some(fi) = flat_idx {
                v /= band_bufs[fi][idx] as f64 / flat_norm_divisor;
            }
            v /= OUTPUT_SCALE_DIVISOR;
            *out_px = v as f32;
        }
        y += rows;
    }

    write_fits_f32(&inputs.output_path, w, h, 1, &out, &inputs.cards)
        .map_err(|e| io_err(format!("writing {}: {e}", inputs.output_path.display())))?;
    let output_hash = compute_xxhash(&inputs.output_path)
        .map_err(|e| io_err(format!("hashing {}: {e:#}", inputs.output_path.display())))?;

    let calstat = if has_flat {
        format!("{calstat_base}F")
    } else {
        calstat_base.to_string()
    };

    tracing::debug!(
        src = %inputs.light_path.display(),
        dest = %inputs.output_path.display(),
        calstat = %calstat,
        flat_norm_divisor,
        width = w,
        height = h,
        "light calibrated"
    );

    Ok(LightCalOutcome {
        calstat,
        flat_norm_divisor,
        output_hash,
    })
}

/// The flat-normalization divisor for `flat_path`: read the `ATH_FNRM` card an
/// Athenaeum-built master stamps, or — for a flat imported without it —
/// recompute the central-third mean on the fly (spec §2), band-reading so
/// memory stays bounded.
pub fn flat_norm_constant(
    flat_path: &Path,
    scratch_dir: &Path,
) -> Result<f64, IntegrationError> {
    if let Some(n) = read_ath_fnrm(flat_path) {
        tracing::debug!(path = %flat_path.display(), ath_fnrm = n, "flat normalization from ATH_FNRM card");
        return Ok(n);
    }

    // Imported master without the card: recompute the central-third mean on the
    // fly (spec §2). Band-read into one plane so memory stays bounded.
    let mut src = BandSource::open(&[flat_path.to_path_buf()], scratch_dir)?;
    let (w, h) = (src.width(), src.height());
    let band_rows = band_rows_for_budget(w, 1, BAND_BUDGET_BYTES).min(h);
    let mut data = vec![0f32; w * h];
    let mut bufs = vec![Vec::new()];
    let mut y = 0;
    while y < h {
        let rows = band_rows.min(h - y);
        src.read_band(y, rows, &mut bufs)?;
        data[y * w..(y + rows) * w].copy_from_slice(&bufs[0]);
        y += rows;
    }
    let mean = central_third_mean(&data, w, h);
    tracing::debug!(path = %flat_path.display(), recomputed = mean, "flat normalization recomputed (ATH_FNRM absent)");
    Ok(mean)
}

/// Best-effort read of a flat's `ATH_FNRM` card. Any failure (unreadable
/// header, card absent, unparseable value) returns `None` so the caller falls
/// back to recomputing the constant.
fn read_ath_fnrm(flat_path: &Path) -> Option<f64> {
    let (_, header_text) = parse_fits_with_header(flat_path, 0).ok()?;
    let keys = parse_stored_header_keys(FileFormat::FITS, &header_text);
    keys.get("ATH_FNRM").and_then(|s| s.parse::<f64>().ok())
}

/// Wrap a non-`IntegrationError` failure (FITS write / hashing) as an IO
/// error so it flows through the engine's single error type.
fn io_err(msg: String) -> IntegrationError {
    IntegrationError::Io(std::io::Error::new(std::io::ErrorKind::Other, msg))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::{Card, CardValue};
    use std::path::Path;

    fn write_plane(dir: &Path, name: &str, w: usize, h: usize, val: f32, cards: &[Card]) -> PathBuf {
        let data = vec![val; w * h];
        let p = dir.join(name);
        write_fits_f32(&p, w, h, 1, &data, cards).unwrap();
        p
    }

    fn write_fill(
        dir: &Path,
        name: &str,
        w: usize,
        h: usize,
        fill: impl Fn(usize, usize) -> f32,
        cards: &[Card],
    ) -> PathBuf {
        let mut data = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                data[y * w + x] = fill(x, y);
            }
        }
        let p = dir.join(name);
        write_fits_f32(&p, w, h, 1, &data, cards).unwrap();
        p
    }

    /// Read a whole f32 FITS back into RAM (one band) for exact assertions.
    fn read_all(path: &Path, scratch: &Path) -> (usize, usize, Vec<f32>) {
        let mut src = BandSource::open(&[path.to_path_buf()], scratch).unwrap();
        let (w, h) = (src.width(), src.height());
        let mut bufs = vec![Vec::new()];
        src.read_band(0, h, &mut bufs).unwrap();
        (w, h, bufs.remove(0))
    }

    fn fnrm_card(v: f64) -> Card {
        Card::new("ATH_FNRM", CardValue::Real(v)).unwrap()
    }

    /// Mirror the engine's per-pixel math exactly (f64 throughout, cast at the
    /// end) so tests can assert bit-exact f32 output.
    fn expect_px(l: f64, s: Option<f64>, f: Option<f64>, divisor: f64) -> f32 {
        let mut v = l;
        if let Some(s) = s {
            v -= s;
        }
        if let Some(f) = f {
            v /= f / divisor;
        }
        v /= OUTPUT_SCALE_DIVISOR;
        v as f32
    }

    #[allow(clippy::too_many_arguments)]
    fn inputs(
        dir: &Path,
        light: PathBuf,
        dark: Option<PathBuf>,
        bias: Option<PathBuf>,
        flat: Option<PathBuf>,
        flat_norm: bool,
        out: PathBuf,
    ) -> LightCalInputs {
        LightCalInputs {
            light_path: light,
            dark_path: dark,
            bias_path: bias,
            flat_path: flat,
            flat_norm,
            output_path: out,
            cards: vec![],
            scratch_dir: dir.to_path_buf(),
        }
    }

    #[test]
    fn full_formula_bdf() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (8usize, 9usize);
        let light = write_plane(dir.path(), "light.fits", w, h, 1100.0, &[]);
        let dark = write_plane(dir.path(), "dark.fits", w, h, 100.0, &[]);
        let flat = write_plane(dir.path(), "flat.fits", w, h, 2.0, &[fnrm_card(2.0)]);
        let out = dir.path().join("out.fits");
        let cfg = inputs(dir.path(), light, Some(dark), None, Some(flat), true, out.clone());

        let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
        assert_eq!(outcome.calstat, "BDF");
        assert!((outcome.flat_norm_divisor - 2.0).abs() < 1e-12);
        assert!(!outcome.output_hash.is_empty());

        let (rw, rh, data) = read_all(&out, dir.path());
        assert_eq!((rw, rh), (w, h));
        // F_norm = 2.0/2.0 = 1.0, so every pixel = (1100-100)/1.0/65535.
        let expected = expect_px(1100.0, Some(100.0), Some(2.0), 2.0);
        assert!(data.iter().all(|&v| v == expected), "got {}, want {expected}", data[0]);
    }

    #[test]
    fn bias_fallback_bf() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (8usize, 9usize);
        let light = write_plane(dir.path(), "light.fits", w, h, 1100.0, &[]);
        let bias = write_plane(dir.path(), "bias.fits", w, h, 50.0, &[]);
        let flat = write_plane(dir.path(), "flat.fits", w, h, 2.0, &[fnrm_card(2.0)]);
        let out = dir.path().join("out.fits");
        let cfg = inputs(dir.path(), light, None, Some(bias), Some(flat), true, out.clone());

        let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
        assert_eq!(outcome.calstat, "BF");

        let (_, _, data) = read_all(&out, dir.path());
        let expected = expect_px(1100.0, Some(50.0), Some(2.0), 2.0);
        assert!(data.iter().all(|&v| v == expected), "got {}, want {expected}", data[0]);
    }

    #[test]
    fn dark_only_bd() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (8usize, 9usize);
        let light = write_plane(dir.path(), "light.fits", w, h, 1100.0, &[]);
        let dark = write_plane(dir.path(), "dark.fits", w, h, 100.0, &[]);
        let out = dir.path().join("out.fits");
        let cfg = inputs(dir.path(), light, Some(dark), None, None, true, out.clone());

        let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
        assert_eq!(outcome.calstat, "BD");
        assert!((outcome.flat_norm_divisor - 1.0).abs() < 1e-12, "no flat -> divisor 1.0");

        let (_, _, data) = read_all(&out, dir.path());
        let expected = expect_px(1100.0, Some(100.0), None, 1.0);
        assert!(data.iter().all(|&v| v == expected), "got {}, want {expected}", data[0]);
    }

    #[test]
    fn flat_only_f() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (8usize, 9usize);
        let light = write_plane(dir.path(), "light.fits", w, h, 1100.0, &[]);
        let flat = write_plane(dir.path(), "flat.fits", w, h, 2.0, &[fnrm_card(2.0)]);
        let out = dir.path().join("out.fits");
        let cfg = inputs(dir.path(), light, None, None, Some(flat), true, out.clone());

        let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
        assert_eq!(outcome.calstat, "F");

        let (_, _, data) = read_all(&out, dir.path());
        let expected = expect_px(1100.0, None, Some(2.0), 2.0);
        assert!(data.iter().all(|&v| v == expected), "got {}, want {expected}", data[0]);
    }

    #[test]
    fn flat_norm_off_changes_scale() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (8usize, 9usize);
        let light = write_plane(dir.path(), "light.fits", w, h, 1100.0, &[]);
        let dark = write_plane(dir.path(), "dark.fits", w, h, 100.0, &[]);
        let flat = write_plane(dir.path(), "flat.fits", w, h, 2.0, &[fnrm_card(2.0)]);
        let out = dir.path().join("out.fits");
        // Same inputs as full_formula_bdf but with normalization OFF.
        let cfg = inputs(dir.path(), light, Some(dark), None, Some(flat), false, out.clone());

        let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
        assert_eq!(outcome.calstat, "BDF", "flat still applied, just not normalized");
        assert!((outcome.flat_norm_divisor - 1.0).abs() < 1e-12, "normalization off -> divisor 1.0");

        let (_, _, data) = read_all(&out, dir.path());
        // divisor 1.0 => divide by (F/1) = 2.0, so half the normalized value.
        let expected_off = expect_px(1100.0, Some(100.0), Some(2.0), 1.0);
        let expected_on = expect_px(1100.0, Some(100.0), Some(2.0), 2.0);
        assert!(data.iter().all(|&v| v == expected_off), "got {}, want {expected_off}", data[0]);
        assert!(
            (data[0] as f64 * 2.0 - expected_on as f64).abs() < 1e-9,
            "flat-norm-off output must be 2x smaller (flat=2.0): {} vs {}",
            data[0],
            expected_on
        );
    }

    #[test]
    fn negatives_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (8usize, 9usize);
        // light < dark => negative numerator, must not be clamped.
        let light = write_plane(dir.path(), "light.fits", w, h, 50.0, &[]);
        let dark = write_plane(dir.path(), "dark.fits", w, h, 100.0, &[]);
        let out = dir.path().join("out.fits");
        let cfg = inputs(dir.path(), light, Some(dark), None, None, true, out.clone());

        let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
        assert_eq!(outcome.calstat, "BD");

        let (_, _, data) = read_all(&out, dir.path());
        let expected = expect_px(50.0, Some(100.0), None, 1.0);
        assert!(expected < 0.0, "fixture must produce a negative expectation");
        assert!(data.iter().all(|&v| v == expected), "got {}, want {expected}", data[0]);
        assert!(data.iter().all(|&v| v < 0.0), "negatives must survive unclamped");
    }

    #[test]
    fn geometry_mismatch_errors() {
        let dir = tempfile::tempdir().unwrap();
        let light = write_plane(dir.path(), "light.fits", 8, 9, 1100.0, &[]);
        // Dark is a different size — BandSource::open must reject the set.
        let dark = write_plane(dir.path(), "dark.fits", 16, 9, 100.0, &[]);
        let out = dir.path().join("out.fits");
        let cfg = inputs(dir.path(), light, Some(dark), None, None, false, out.clone());

        let r = calibrate_light(&cfg, &AtomicBool::new(false));
        assert!(matches!(r, Err(IntegrationError::BadInput(_))), "expected BadInput, got {r:?}");
        assert!(!out.exists(), "no output on a geometry error");
    }

    #[test]
    fn fnrm_recomputed_when_card_missing() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (9usize, 9usize);
        // Column gradient, NO ATH_FNRM card -> constant must be recomputed.
        let fill = |x: usize, _y: usize| 100.0 + x as f32;
        let flat = write_fill(dir.path(), "flat_nocard.fits", w, h, fill, &[]);
        let mut fdata = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                fdata[y * w + x] = fill(x, y);
            }
        }
        let expected = central_third_mean(&fdata, w, h);
        assert!(expected > 100.0, "central-third mean must reflect the gradient");

        let got = flat_norm_constant(&flat, dir.path()).unwrap();
        assert!((got - expected).abs() < 1e-9, "recomputed {got}, want central-third mean {expected}");

        // And when the card IS present it takes precedence over recomputation,
        // even with identical pixel data (value differs from the mean above).
        let flat_carded = write_fill(dir.path(), "flat_carded.fits", w, h, fill, &[fnrm_card(999.0)]);
        let carded = flat_norm_constant(&flat_carded, dir.path()).unwrap();
        assert!((carded - 999.0).abs() < 1e-9, "ATH_FNRM card must win over recomputation, got {carded}");
    }

    #[test]
    fn cancel_mid_run_returns_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (32usize, 32usize);
        let light = write_plane(dir.path(), "light.fits", w, h, 1100.0, &[]);
        let dark = write_plane(dir.path(), "dark.fits", w, h, 100.0, &[]);
        let out = dir.path().join("out.fits");
        let cfg = inputs(dir.path(), light, Some(dark), None, None, true, out.clone());

        // Pre-set: the first per-band cancel check trips before any write.
        let cancel = AtomicBool::new(true);
        let r = calibrate_light(&cfg, &cancel);
        assert!(matches!(r, Err(IntegrationError::Cancelled)), "expected Cancelled, got {r:?}");
        assert!(!out.exists(), "cancelled run must leave no output file");
    }
}

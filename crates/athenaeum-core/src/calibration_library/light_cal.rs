//! Band-streaming light-calibration engine (B5, design spec
//! 2026-07-05-light-calibration-design.md §2). Applies a master dark (or bias)
//! and a master flat to one LIGHT frame, producing a calibrated 32-bit-float
//! FITS with negatives preserved (no clamping, no pedestal).
//!
//! Math (spec §2, verbatim):
//! ```text
//! L_c = ((L − S) / (F / divisor)) / OUTPUT_SCALE_DIVISOR + pedestal_dn / OUTPUT_SCALE_DIVISOR
//! ```
//! where `S` = master dark if linked, else master bias, else no subtraction;
//! `F` = master flat when linked (division skipped otherwise); `divisor` =
//! the flat-normalization constant when normalization is on, else `1.0`;
//! `OUTPUT_SCALE_DIVISOR = 65535.0` normalizes 16-bit sources to ~[0,1]; and
//! `pedestal_dn` (advanced param, default 0 = off) is a DN offset added AFTER
//! the scale divide for consumers that clip negatives.
//!
//! The flat denominator is floored at [`FLAT_DENOM_FLOOR`] so a dead, negative,
//! or non-finite flat pixel cannot produce Inf/NaN or flip the light's sign;
//! the hits are counted into [`LightCalOutcome::floored_flat_pixels`] and
//! warned about, never turned into a frame failure.
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

// The shared light-calibration data types + consts (LIGHT_CAL_ENGINE_VERSION,
// PI_TRIM_FRACTION, FlatNormMode, BiasFallback, LightCalParams) live in
// `crate::models` so ungated consumers (db::light_calibrations, scanner,
// export) compile with `--no-default-features`. They are re-exported here so
// this engine and its render-gated callers keep using the `light_cal::` paths.
pub use crate::models::{
    BiasFallback, FlatNormMode, LightCalParams, LIGHT_CAL_ENGINE_VERSION, PI_TRIM_FRACTION,
};

/// Divisor that normalizes a 16-bit source's counts to roughly `[0, 1]`
/// (spec §2; stamped into the output header as `ATH_CSCL`). Kept as a single
/// constant so the scale decision lives in exactly one place. Only the
/// render-gated engine uses it, so it stays here rather than in `models`.
pub const OUTPUT_SCALE_DIVISOR: f64 = 65535.0;

/// Floor for the flat denominator in normalized units. A dead / negative flat
/// pixel must not produce Inf/NaN or flip the light's sign; established
/// stacking tools floor this division the same way and count the hits.
pub const FLAT_DENOM_FLOOR: f64 = 2.0e-5;

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
    /// Which statistic computes the normalization divisor when `flat_norm` is on
    /// (spec §2). Ignored when `flat_norm` is `false` or no flat is applied.
    pub flat_norm_mode: FlatNormMode,
    /// Advanced per-run parameters (spec §2). The engine acts on
    /// `trim_fraction` (feeds `pixinsightTrimmed` normalization) and
    /// `pedestal_dn` (added after the scale divide); `bias_fallback` is enforced
    /// by the orchestration layer BEFORE the engine runs, so the engine is
    /// agnostic to it.
    pub params: LightCalParams,
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
    /// How many pixels hit [`FLAT_DENOM_FLOOR`] because the flat's value there
    /// was dead, negative, or non-finite. Reported/logged only — a floored
    /// pixel is a warning, never a frame failure.
    pub floored_flat_pixels: u64,
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
        (Some(flat), true) => flat_norm_constant(
            flat,
            &inputs.scratch_dir,
            inputs.flat_norm_mode,
            inputs.params.trim_fraction,
        )?,
        _ => 1.0,
    };

    // Output pedestal (spec §2): DN added AFTER the scale divide. Precomputed in
    // output units once; the add is skipped entirely when the pedestal is 0.
    let add_pedestal = inputs.params.pedestal_dn != 0.0;
    let pedestal_offset = inputs.params.pedestal_dn / OUTPUT_SCALE_DIVISOR;

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

    // Dead/negative flat pixels that hit FLAT_DENOM_FLOOR. The band loop is
    // serial, so a plain counter is correct.
    let mut floored_flat_pixels: u64 = 0;

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
                // A dead (0.0), negative, or non-finite flat pixel would make
                // this division Inf/NaN or flip the light's sign. Floor the
                // denominator instead and count the hit — the frame stays
                // usable and the count is warned about below. A master flat
                // built here writes an all-undefined pixel as exactly 0.0, so
                // the zero case is reachable, not hypothetical.
                let denom = band_bufs[fi][idx] as f64 / flat_norm_divisor;
                if denom.is_finite() && denom >= FLAT_DENOM_FLOOR {
                    v /= denom;
                } else {
                    v /= FLAT_DENOM_FLOOR;
                    floored_flat_pixels += 1;
                }
            }
            v /= OUTPUT_SCALE_DIVISOR;
            if add_pedestal {
                v += pedestal_offset;
            }
            *out_px = v as f32;
        }
        y += rows;
    }

    if floored_flat_pixels > 0 {
        tracing::warn!(
            src = %inputs.light_path.display(),
            count = floored_flat_pixels,
            "flat denominator floored (dead/negative flat pixels)"
        );
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
        floored_flat_pixels,
        "light calibrated"
    );

    Ok(LightCalOutcome {
        calstat,
        flat_norm_divisor,
        output_hash,
        floored_flat_pixels,
    })
}

/// The flat-normalization divisor for `flat_path` under `mode` (spec §2):
///
/// - [`FlatNormMode::CentralThird`] (default, Athenaeum convention): read the
///   `ATH_FNRM` card an Athenaeum-built master stamps, or — for a flat imported
///   without it — recompute the central-third mean on the fly.
/// - [`FlatNormMode::PixinsightTrimmed`] (PixInsight-compatible): a two-sided
///   trimmed mean over the WHOLE frame (`trim_fraction` per tail, default
///   [`PI_TRIM_FRACTION`]). ALWAYS computed from the flat file; the `ATH_FNRM`
///   card is ignored.
///
/// `trim_fraction` is only consulted in `PixinsightTrimmed` mode (the
/// central-third path never trims); callers in central-third mode may pass
/// [`PI_TRIM_FRACTION`].
///
/// Whatever the mode, the resolved constant must be finite and strictly
/// positive to serve as a divisor — anything else is
/// [`IntegrationError::BadInput`]. An `ATH_FNRM` card that fails that test
/// counts as absent and falls through to recomputation.
///
/// Both paths band-read the flat one row-band at a time, so memory stays
/// bounded regardless of frame size.
pub fn flat_norm_constant(
    flat_path: &Path,
    scratch_dir: &Path,
    mode: FlatNormMode,
    trim_fraction: f64,
) -> Result<f64, IntegrationError> {
    let n = match mode {
        FlatNormMode::CentralThird => {
            if let Some(n) = read_ath_fnrm(flat_path) {
                tracing::debug!(path = %flat_path.display(), ath_fnrm = n, "flat normalization from ATH_FNRM card");
                n
            } else {
                // Imported master without a usable card: recompute the
                // central-third mean.
                let (w, h, data) = read_full_flat_plane(flat_path, scratch_dir)?;
                let mean = central_third_mean(&data, w, h);
                tracing::debug!(path = %flat_path.display(), recomputed = mean, "flat normalization recomputed (ATH_FNRM absent)");
                mean
            }
        }
        FlatNormMode::PixinsightTrimmed => {
            // PixInsight parity: the card's meaning (central-third) does not
            // match this statistic, so it is deliberately ignored — always
            // computed from the flat's pixels.
            let (_w, _h, data) = read_full_flat_plane(flat_path, scratch_dir)?;
            let mean = pixinsight_trimmed_mean(&data, trim_fraction);
            tracing::debug!(path = %flat_path.display(), trimmed_mean = mean, trim_fraction, "flat normalization from full-frame trimmed mean (pixinsightTrimmed)");
            mean
        }
    };
    validate_norm_constant(n, flat_path)
}

/// A normalization constant is only meaningful as a divisor when it is finite
/// and strictly positive. A degenerate flat (all-zero, NaN-filled) would
/// otherwise silently scale the whole frame to Inf/NaN, so it fails the frame
/// loudly instead — per-frame, the batch continues.
fn validate_norm_constant(n: f64, flat_path: &Path) -> Result<f64, IntegrationError> {
    if !(n.is_finite() && n > 0.0) {
        return Err(IntegrationError::BadInput(format!(
            "flat normalization constant {n} is not a positive finite number ({})",
            flat_path.display()
        )));
    }
    Ok(n)
}

/// Band-read the entire flat plane into one `Vec<f32>` (bounded memory: one
/// row-band at a time, assembled into the single output plane). Shared by both
/// [`FlatNormMode`] paths that need the raw pixels.
fn read_full_flat_plane(
    flat_path: &Path,
    scratch_dir: &Path,
) -> Result<(usize, usize, Vec<f32>), IntegrationError> {
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
    Ok((w, h, data))
}

/// Two-sided trimmed mean over the whole plane, discarding exactly
/// `trim_fraction` of the pixels from EACH tail (PixInsight's
/// `flatScaleClippingFactor` semantics, spec §2; `trim_fraction` defaults to
/// [`PI_TRIM_FRACTION`], the advanced param exposes it). Sorts a copy (total
/// order, NaN-tolerant) and averages the surviving middle in f64:
/// `lo = floor(n·f)`, `hi = n − floor(n·f)`, mean over `sorted[lo..hi]`.
///
/// Degenerate guards: an empty plane returns `1.0` (a harmless divide-by later),
/// and a frame so small that `lo >= hi` falls back to the full-frame mean rather
/// than averaging an empty slice.
fn pixinsight_trimmed_mean(data: &[f32], trim_fraction: f64) -> f64 {
    let n = data.len();
    if n == 0 {
        return 1.0;
    }
    let mut sorted: Vec<f32> = data.to_vec();
    sorted.sort_unstable_by(|a, b| a.total_cmp(b));
    let trim = ((n as f64) * trim_fraction).floor() as usize;
    let (lo, hi) = if trim * 2 < n { (trim, n - trim) } else { (0, n) };
    let slice = &sorted[lo..hi];
    let sum: f64 = slice.iter().map(|&v| v as f64).sum();
    sum / (slice.len() as f64)
}

/// Best-effort read of a flat's `ATH_FNRM` card. Any failure (unreadable
/// header, card absent, unparseable value) returns `None` so the caller falls
/// back to recomputing the constant. A card that parses but cannot serve as a
/// divisor (non-finite, zero, negative) counts as absent for the same reason —
/// recomputing from the pixels beats trusting a broken stamp.
fn read_ath_fnrm(flat_path: &Path) -> Option<f64> {
    let (_, header_text) = parse_fits_with_header(flat_path, 0).ok()?;
    let keys = parse_stored_header_keys(FileFormat::FITS, &header_text);
    keys.get("ATH_FNRM")
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|n| n.is_finite() && *n > 0.0)
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
        expect_px_ped(l, s, f, divisor, 0.0)
    }

    /// [`expect_px`] with an output pedestal (DN) added after the scale divide,
    /// mirroring the engine's `add_pedestal` step exactly (f64 throughout).
    fn expect_px_ped(l: f64, s: Option<f64>, f: Option<f64>, divisor: f64, pedestal_dn: f64) -> f32 {
        let mut v = l;
        if let Some(s) = s {
            v -= s;
        }
        if let Some(f) = f {
            v /= f / divisor;
        }
        v /= OUTPUT_SCALE_DIVISOR;
        if pedestal_dn != 0.0 {
            v += pedestal_dn / OUTPUT_SCALE_DIVISOR;
        }
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
            flat_norm_mode: FlatNormMode::CentralThird,
            params: LightCalParams::default(),
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

        let got = flat_norm_constant(&flat, dir.path(), FlatNormMode::CentralThird, PI_TRIM_FRACTION).unwrap();
        assert!((got - expected).abs() < 1e-9, "recomputed {got}, want central-third mean {expected}");

        // And when the card IS present it takes precedence over recomputation,
        // even with identical pixel data (value differs from the mean above).
        let flat_carded = write_fill(dir.path(), "flat_carded.fits", w, h, fill, &[fnrm_card(999.0)]);
        let carded = flat_norm_constant(&flat_carded, dir.path(), FlatNormMode::CentralThird, PI_TRIM_FRACTION).unwrap();
        assert!((carded - 999.0).abs() < 1e-9, "ATH_FNRM card must win over recomputation, got {carded}");
    }

    #[test]
    fn pixinsight_trimmed_mean_exact() {
        // 1000 distinct values 0..=999 → trim floor(1000*0.05)=50 from each
        // tail → mean of sorted[50..950] = mean(50..=949) = (50+949)/2 = 499.5.
        let vals: Vec<f32> = (0..1000).map(|i| i as f32).collect();
        let m = pixinsight_trimmed_mean(&vals, PI_TRIM_FRACTION);
        assert!((m - 499.5).abs() < 1e-9, "trimmed mean {m}, want 499.5");

        // Order-independent: shuffling the input must not change the statistic
        // (the fn sorts internally).
        let mut rev: Vec<f32> = vals.clone();
        rev.reverse();
        assert!((pixinsight_trimmed_mean(&rev, PI_TRIM_FRACTION) - 499.5).abs() < 1e-9);
    }

    #[test]
    fn trim_fraction_changes_divisor() {
        // A right-skewed distribution (quadratic ramp) so a wider trim actually
        // changes the two-sided trimmed mean — a symmetric set would give the
        // same statistic at any fraction.
        let data: Vec<f32> = (0..1000).map(|i| (i * i) as f32).collect();
        let m05 = pixinsight_trimmed_mean(&data, 0.05);
        let m10 = pixinsight_trimmed_mean(&data, 0.10);
        assert!(
            (m05 - m10).abs() > 1.0,
            "a wider trim must change the skewed statistic: {m05} vs {m10}"
        );

        // The 0.10 value equals the exact formula: lo=floor(1000*0.10)=100,
        // hi=900, mean over the already-sorted middle 800 samples.
        let expected: f64 = data[100..900].iter().map(|&v| v as f64).sum::<f64>() / 800.0;
        assert!((m10 - expected).abs() < 1e-6, "trim 0.10 {m10}, want {expected}");

        // And the engine's band-read path honors the fraction end-to-end.
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (40usize, 25usize); // 1000 pixels, values 0..=999
        let flat = write_fill(dir.path(), "flat_skew.fits", w, h, |x, y| {
            let i = (y * w + x) as f32;
            i * i
        }, &[]);
        let got = flat_norm_constant(&flat, dir.path(), FlatNormMode::PixinsightTrimmed, 0.10).unwrap();
        assert!((got - expected).abs() < 1e-2, "engine trim-0.10 path {got} vs formula {expected}");
    }

    #[test]
    fn pedestal_adds_after_scale_divide() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (8usize, 9usize);
        let light = write_plane(dir.path(), "light.fits", w, h, 1100.0, &[]);
        let dark = write_plane(dir.path(), "dark.fits", w, h, 100.0, &[]);

        // Zero-pedestal run must be bit-identical to the pre-feature output.
        let out0 = dir.path().join("out0.fits");
        let cfg0 = inputs(dir.path(), light.clone(), Some(dark.clone()), None, None, true, out0.clone());
        calibrate_light(&cfg0, &AtomicBool::new(false)).unwrap();
        let (_, _, d0) = read_all(&out0, dir.path());
        let base = expect_px(1100.0, Some(100.0), None, 1.0);
        assert!(d0.iter().all(|&v| v == base), "zero pedestal must match legacy output");

        // +100 DN must add exactly 100/65535 to every pixel (in f64, cast once).
        let out1 = dir.path().join("out1.fits");
        let mut cfg1 = inputs(dir.path(), light, Some(dark), None, None, true, out1.clone());
        cfg1.params.pedestal_dn = 100.0;
        calibrate_light(&cfg1, &AtomicBool::new(false)).unwrap();
        let (_, _, d1) = read_all(&out1, dir.path());
        let expected = expect_px_ped(1100.0, Some(100.0), None, 1.0, 100.0);
        assert!(d1.iter().all(|&v| v == expected), "got {}, want {expected}", d1[0]);
        assert!(
            (expected as f64 - base as f64 - 100.0 / OUTPUT_SCALE_DIVISOR).abs() < 1e-9,
            "pedestal must shift output by exactly pedestal_dn/scale"
        );
    }

    #[test]
    fn pixinsight_trimmed_mode_ignores_fnrm_card() {
        // A 40x25 = 1000-pixel plane of the distinct values 0..=999, stamped
        // with a BOGUS ATH_FNRM card. PixinsightTrimmed must IGNORE the card and
        // return the trimmed mean 499.5; CentralThird must read the card (999.0).
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (40usize, 25usize);
        let flat = write_fill(dir.path(), "flat_trim.fits", w, h, |x, y| (y * w + x) as f32, &[fnrm_card(999.0)]);

        let trimmed = flat_norm_constant(&flat, dir.path(), FlatNormMode::PixinsightTrimmed, PI_TRIM_FRACTION).unwrap();
        assert!((trimmed - 499.5).abs() < 1e-6, "trimmed {trimmed}, want 499.5 (card ignored)");

        let central = flat_norm_constant(&flat, dir.path(), FlatNormMode::CentralThird, PI_TRIM_FRACTION).unwrap();
        assert!((central - 999.0).abs() < 1e-9, "central-third must read the card verbatim, got {central}");
    }

    #[test]
    fn pixinsight_trimmed_mean_real_shape_matches_formula() {
        // A realistic (NaN-free) radial-vignetting flat: bright center, dimmer
        // corners. The engine's band-read path must produce exactly the same
        // trimmed mean as computing the formula directly over the pixel values.
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (64usize, 48usize);
        let cx = (w as f32 - 1.0) / 2.0;
        let cy = (h as f32 - 1.0) / 2.0;
        let fill = |x: usize, y: usize| {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let r2 = (dx * dx + dy * dy) / (cx * cx + cy * cy);
            20000.0 * (1.0 - 0.3 * r2)
        };
        let flat = write_fill(dir.path(), "flat_vig.fits", w, h, fill, &[]);

        let mut data = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                data[y * w + x] = fill(x, y);
            }
        }
        assert!(data.iter().all(|v| v.is_finite()), "fixture must be NaN-free");
        let expected = pixinsight_trimmed_mean(&data, PI_TRIM_FRACTION);
        // Sanity: the trimmed mean sits strictly inside the value range.
        let (mn, mx) = data.iter().fold((f32::MAX, f32::MIN), |(a, b), &v| (a.min(v), b.max(v)));
        assert!((mn as f64) < expected && expected < (mx as f64), "trimmed mean {expected} outside ({mn},{mx})");

        let got = flat_norm_constant(&flat, dir.path(), FlatNormMode::PixinsightTrimmed, PI_TRIM_FRACTION).unwrap();
        assert!((got - expected).abs() < 1e-3, "engine path {got} vs direct formula {expected}");
    }

    /// Run the engine over a 2x2 light of 100.0 divided by an explicit 4-pixel
    /// flat plane, returning the outcome plus the written pixels. With
    /// `flat_norm` off the divisor stays 1.0, so `flat_px` ARE the denominators.
    fn run_engine_with_flat(flat_px: &[f32], flat_norm: bool) -> (LightCalOutcome, Vec<f32>) {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (2usize, 2usize);
        assert_eq!(flat_px.len(), w * h, "fixture must be a full 2x2 plane");
        let light = write_plane(dir.path(), "light.fits", w, h, 100.0, &[]);
        let flat = write_fill(dir.path(), "flat.fits", w, h, |x, y| flat_px[y * w + x], &[]);
        let out = dir.path().join("out.fits");
        let cfg = inputs(dir.path(), light, None, None, Some(flat), flat_norm, out.clone());

        let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
        let (_, _, data) = read_all(&out, dir.path());
        (outcome, data)
    }

    #[test]
    fn zero_and_negative_flat_pixels_are_floored_not_inf() {
        // Pixel 0 = 0.0 is the master-build seam: a pixel whose every sample was
        // rejected as non-finite arrives here as 0.0 in a built master flat.
        // Pixel 1 = -0.5 is the sign-flip case. Both must floor, neither may
        // produce Inf/NaN or turn a positive light negative.
        let (outcome, data) = run_engine_with_flat(&[0.0, -0.5, 1.0, 1.0], false);
        assert_eq!(outcome.calstat, "F");
        assert!(data.iter().all(|v| v.is_finite()), "no Inf/NaN survives the floor: {data:?}");
        assert!(data[0] > 0.0 && data[1] > 0.0, "no sign flips: {} {}", data[0], data[1]);
        assert_eq!(outcome.floored_flat_pixels, 2, "exactly the dead + negative pixels");

        // Both floored pixels divide by the same FLAT_DENOM_FLOOR, so they land
        // on the identical value; the healthy pixels are untouched.
        let floored = expect_px(100.0, None, Some(FLAT_DENOM_FLOOR), 1.0);
        assert_eq!(data[0], floored, "dead flat pixel must divide by the floor");
        assert_eq!(data[1], floored, "negative flat pixel must divide by the floor");
        let healthy = expect_px(100.0, None, Some(1.0), 1.0);
        assert_eq!(data[2], healthy);
        assert_eq!(data[3], healthy);
    }

    #[test]
    fn healthy_flat_never_counts_a_floored_pixel() {
        // Regression guard on the counter: a flat whose smallest pixel is still
        // above the floor must report zero hits (the warn must stay silent).
        let (outcome, data) = run_engine_with_flat(&[1.0, 0.5, 2.0, 1e-4], false);
        assert_eq!(outcome.floored_flat_pixels, 0);
        assert!(data.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn bogus_ath_fnrm_card_falls_back_to_recompute() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (9usize, 9usize);
        let fill = |x: usize, _y: usize| 100.0 + x as f32;
        // A non-positive ATH_FNRM is unusable as a divisor — treated as absent,
        // so the constant is recomputed from the pixels instead.
        let flat = write_fill(dir.path(), "flat_zero_card.fits", w, h, fill, &[fnrm_card(0.0)]);
        let mut fdata = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                fdata[y * w + x] = fill(x, y);
            }
        }
        let expected = central_third_mean(&fdata, w, h);

        let got = flat_norm_constant(&flat, dir.path(), FlatNormMode::CentralThird, PI_TRIM_FRACTION).unwrap();
        assert!((got - expected).abs() < 1e-9, "bogus card must fall through to recompute: {got} vs {expected}");
    }

    #[test]
    fn non_positive_normalization_constant_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (9usize, 9usize);
        // An all-zero flat: no card, and the recomputed mean is 0.0 — dividing
        // by it is meaningless, so the run fails loudly instead of silently
        // producing Inf.
        let flat = write_plane(dir.path(), "flat_dead.fits", w, h, 0.0, &[]);
        let r = flat_norm_constant(&flat, dir.path(), FlatNormMode::CentralThird, PI_TRIM_FRACTION);
        assert!(matches!(r, Err(IntegrationError::BadInput(_))), "expected BadInput, got {r:?}");
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

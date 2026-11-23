//! Band-streaming light-calibration engine (B5, design spec
//! 2026-07-05-light-calibration-design.md §2). Applies a master dark (or bias)
//! and a master flat to one LIGHT frame, producing a calibrated 32-bit-float
//! FITS with negatives preserved (no clamping, no pedestal).
//!
//! Math (spec §2, verbatim):
//! ```text
//! L_c = ((L − S) / (F / divisor)) / scale_divisor + pedestal_dn / scale_divisor
//! ```
//! where `S` = master dark if linked, else master bias, else no subtraction;
//! `F` = master flat when linked (division skipped otherwise); `divisor` =
//! the flat-normalization constant when normalization is on, else `1.0` — one
//! number for the whole plane, or one per CFA colour when the light is a mosaic
//! and per-channel scaling applies ([`resolve_flat_norm_divisor`] owns that
//! choice, [`FlatNormDivisor`] carries it);
//! `scale_divisor` is the SOURCE's bit-depth maximum
//! ([`scale_divisor_for_bitpix`], resolved by the caller — `65535.0` for the
//! common 16-bit case, `1.0` for an already-physical float source) so the
//! output lands in about 0 to 1 whatever the light was stored as; and
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
//! built by the caller and handed
//! in via [`LightCalInputs::cards`]; the output path is resolved by the
//! orchestration layer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::fits_writer::Card;
use crate::integration::band_budget::MIN_BUDGET_BYTES;
use crate::integration::banded::{BandPlanes, BandSource};
use crate::integration::cfa::{cfa_channel_at, central_third_channel_means, CfaGeometry};
use crate::integration::engine::central_third_mean;
use crate::integration::IntegrationError;

// The parameter types live in `calibration_library::params` (the base layer) and are re-exported here so the
// `light_cal::` paths keep working. `LIGHT_CAL_ENGINE_VERSION` stays with the application (spec SL-19).
pub use crate::calibration_library::params::{BiasFallback, FlatNormMode, LightCalParams, TRIMMED_MEAN_TAIL_FRACTION};

/// Where a master flat's stamped normalization constants come from. The math asks this at exactly the two
/// places it can use a stamped constant: the per-channel constants ([`resolve_flat_norm_divisor_with`]) and the
/// global one ([`flat_norm_constant_with`]). An implementation that answers `None` makes the math recompute from
/// the flat's pixels, which is what a flat without stamped constants gets.
pub trait FlatCardReader {
    /// The flat's stamped per-channel constants `[R, G, B]`, usable as divisors under the light's mosaic phase,
    /// or `None` (absent, unusable, or measured under another phase).
    fn channel_norms(&self, flat: &Path, light: CfaGeometry) -> Option<[f64; 3]>;
    /// The flat's stamped global constant, usable as a divisor, or `None`.
    fn fnrm(&self, flat: &Path) -> Option<f64>;
}

/// The reader that stamps nothing: every constant is recomputed from the flat's pixels.
pub struct NoFlatCards;

impl FlatCardReader for NoFlatCards {
    fn channel_norms(&self, _flat: &Path, _light: CfaGeometry) -> Option<[f64; 3]> {
        None
    }
    fn fnrm(&self, _flat: &Path) -> Option<f64> {
        None
    }
}

/// [`scale_divisor_for_bitpix`], which owns the per-source decision. Only the
/// render-gated engine uses it, so it stays here rather than in `models`.
pub const OUTPUT_SCALE_DIVISOR: f64 = 65535.0;

/// Output scale divisor for a source of the given BITPIX (spec §2: "the source
/// bit-depth maximum"), so an 8-bit or 32-bit-integer light is not silently
/// mis-scaled by the 16-bit constant. Float sources (`-32`/`-64`) already carry
/// physical values and are passed through unscaled (`1.0`). An unknown depth —
/// `None` from [`crate::integration::banded::probe_bitpix`] for the
/// decode-and-spill formats, or a BITPIX outside the FITS set — keeps the
/// historic [`OUTPUT_SCALE_DIVISOR`] rather than guessing.
pub fn scale_divisor_for_bitpix(bitpix: Option<i32>) -> f64 {
    match bitpix {
        Some(8) => 255.0,
        Some(16) => 65535.0,
        Some(32) => 4294967295.0,
        Some(-32) | Some(-64) => 1.0,
        _ => OUTPUT_SCALE_DIVISOR,
    }
}

/// Floor for the flat denominator in normalized units. A dead / negative flat
/// pixel must not produce Inf/NaN or flip the light's sign; established
/// stacking tools floor this division the same way and count the hits.
///
/// The floor's reach is mode-dependent: with flat normalization ON the
/// denominator is `flat / <global normalization constant>` (values around 1.0, so the floor only
/// catches genuinely dead/negative pixels), while with normalization OFF the
/// denominator is the RAW flat value — at typical flat levels nothing but a
/// zero, negative, or non-finite pixel can reach it. So
/// [`LightCalOutcome::floored_flat_pixels`] is not comparable across the two
/// modes. Per-channel scaling does not change the floor's reach: the
/// denominator is still `flat / <constant>`, only the constant differs by
/// colour, and each channel's normalizes around 1.0 the same way.
pub const FLAT_DENOM_FLOOR: f64 = 2.0e-5;

/// Everything the engine needs to calibrate one LIGHT frame.
///
/// `dark_path`/`bias_path` are the master subtrahend candidates — a dark is
/// preferred when both are present (raw-master-dark convention removes bias
/// and dark in one subtraction). `flat_path` is the illumination master.
/// `cards` is the fully-built output header (from the caller's header builder);
/// the engine writes it through verbatim and makes no assumptions about it.
pub struct LightCalInputs {
    /// The light frame to calibrate (FITS or XISF).
    pub light_path: PathBuf,
    /// The master dark, in raw sensor DN, subtracted when present; preferred over the bias when both are given.
    pub dark_path: Option<PathBuf>,
    /// The master bias, in raw sensor DN, subtracted only when there is no dark.
    pub bias_path: Option<PathBuf>,
    /// The master flat (DN) that corrects the light's illumination; `None` skips the flat division.
    pub flat_path: Option<PathBuf>,
    /// Normalize the master flat by its global normalization constant (spec §2 toggle,
    /// default on). `false` → plain division by the flat as stored.
    pub flat_norm: bool,
    /// Which statistic computes the normalization divisor when `flat_norm` is on
    /// (spec §2). Ignored when `flat_norm` is `false` or no flat is applied.
    pub flat_norm_mode: FlatNormMode,
    /// The LIGHT frame's own mosaic phase, when it declares one the catalog can
    /// vouch for. `None` = mono (or an unrecognized `BAYERPAT`), which makes
    /// per-channel flat scaling inapplicable however
    /// [`LightCalParams::cfa_flat_scaling`] is set.
    ///
    /// Pure data: the *policy* (is per-channel scaling wanted, is the mode
    /// eligible, do the constants hold up) lives in
    /// [`resolve_flat_norm_divisor`], so the engine and the orchestration layer
    /// — which resolves the same divisor a second time to stamp the header —
    /// can never disagree about what was applied.
    pub cfa_geometry: Option<CfaGeometry>,
    /// Advanced per-run parameters (spec §2). The engine acts on
    /// `trim_fraction` (feeds `TrimmedMeanWhole` normalization) and
    /// `pedestal_dn` (added after the scale divide); `bias_fallback` is enforced
    /// by the orchestration layer BEFORE the engine runs, so the engine is
    /// agnostic to it.
    pub params: LightCalParams,
    /// Divisor applied AFTER the calibration arithmetic to bring the output to
    /// about 0 to 1 — the light source's own bit-depth maximum, resolved by the
    /// caller via [`scale_divisor_for_bitpix`] (the engine never re-reads the
    /// source header for it) and recorded in the output header by the caller's header builder,
    /// so the written card and the applied value are the same number.
    pub scale_divisor: f64,
    /// Where the calibrated file is written (by the write phase; the compute phase never touches it).
    pub output_path: PathBuf,
    /// The complete output header, written through verbatim.
    pub cards: Vec<Card>,
    /// A folder for the spill files of source formats that must be decoded to disk before they can be band-read.
    pub scratch_dir: PathBuf,
}

/// How the master flat's normalization divisor is applied across the frame.
///
/// One value for the whole plane ([`FlatNormDivisor::Global`], the mono and
/// pre-CFA behavior), or one per mosaic colour
/// ([`FlatNormDivisor::PerChannel`]) so a CFA flat's own colour response is
/// divided out of each channel separately instead of tinting the result.
///
/// Resolved once by [`resolve_flat_norm_divisor`] — a frame is never
/// mixed-mode: if any channel constant fails to hold up, the WHOLE frame falls
/// back to `Global`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FlatNormDivisor {
    /// One constant for the whole plane, in the flat's own units (DN for a raw flat).
    Global(f64),
    /// One constant per mosaic color, in the flat's own units (DN for a raw flat).
    PerChannel {
        /// The light's mosaic phase the constants were measured under.
        geom: CfaGeometry,
        /// The constants for red, green and blue, in that order.
        k: [f64; 3]
    },
}

impl FlatNormDivisor {
    /// The single number that describes this divisor for the tracking row and
    /// the caller's header stamp of the applied divisor: the constant itself in `Global` mode, and in
    /// `PerChannel` mode the constants blended **by their share of the mosaic** —
    /// `(R + 2G + B) / 4`, since a Bayer cell is half green. That weighting is
    /// what makes the number continuous with the global constant it stands in
    /// for: the global central-third mean averages the window's pixels, which
    /// are green half the time, so an unweighted `(R+G+B)/3` would sit somewhere
    /// the global path never produces. Per-channel runs also stamp the three
    /// constants individually, so this
    /// remains a continuity value, not the whole story.
    pub fn global_value(self) -> f64 {
        match self {
            FlatNormDivisor::Global(k) => k,
            FlatNormDivisor::PerChannel { k, .. } => (k[0] + 2.0 * k[1] + k[2]) / 4.0,
        }
    }

    /// The `[R, G, B]` constants when scaling per channel, else `None`.
    pub fn channel_values(self) -> Option<[f64; 3]> {
        match self {
            FlatNormDivisor::Global(_) => None,
            FlatNormDivisor::PerChannel { k, .. } => Some(k),
        }
    }

    /// Whether this divisor holds one constant per CFA color rather than one for the whole plane.
    pub fn is_per_channel(self) -> bool {
        matches!(self, FlatNormDivisor::PerChannel { .. })
    }
}

/// What the engine actually applied, for the tracking row + progress summary.
#[derive(Debug, Clone, PartialEq)]
pub struct LightCalOutcome {
    /// Applied-state flags (`"BDF"`, `"BF"`, `"BD"`, `"B"`, `"F"`).
    pub calstat: String,
    /// Divisor actually used for flat normalization: the flat's global normalization constant when
    /// normalization was on, else `1.0` (also `1.0` when no flat was applied).
    /// In per-channel mode this is the mosaic-weighted blend of the three
    /// channel constants ([`FlatNormDivisor::global_value`]) — the
    /// global-equivalent number, kept so the row column and the caller's header stamp
    /// mean the same thing in both modes.
    pub flat_norm_divisor: f64,
    /// The `[R, G, B]` constants when the flat was normalized per CFA channel,
    /// else `None`. `Some` implies [`LightCalOutcome::cfa_scaling_applied`].
    pub flat_channel_divisors: Option<[f64; 3]>,
    /// Whether per-channel scaling was ACTUALLY applied — not merely requested.
    /// `false` for a mono light, a `TrimmedMeanWhole` run, a flat whose channel
    /// constants came back degenerate, or the toggle simply being off. This is
    /// what the tracking row records.
    pub cfa_scaling_applied: bool,
    /// xxh3 of the written output file.
    pub output_hash: String,
    /// How many pixels hit [`FLAT_DENOM_FLOOR`] because the flat's value there
    /// was dead, negative, or non-finite. Reported/logged only — a floored
    /// pixel is a warning, never a frame failure.
    pub floored_flat_pixels: u64,
    /// How many pixels were at or below zero right after the dark or bias was
    /// subtracted (before the flat, the scale divide and the output pedestal;
    /// none of those can change a pixel's sign). `0` when nothing was
    /// subtracted. A counter only: the output bytes are the same with or
    /// without it. A healthy light's median after subtraction is its sky,
    /// which is above zero, so a count near [`LightCalOutcome::pixels`] means
    /// the dark is brighter than the light.
    pub non_positive_pixels: u64,
    /// The pixels the counter looked at: `width * height` when a dark or bias
    /// was subtracted, `0` when none was. The denominator of
    /// [`LightCalOutcome::non_positive_pixels`].
    pub pixels: u64,
}

/// One calibrated image plane held in RAM, handed from the compute phase to the
/// write phase.
///
/// The split exists so intermediate pixel stages can run between the formula
/// and the file write without either phase knowing about them. `data` is
/// row-major and `width * height` long — a stage that turns one plane into
/// several hands its own buffer straight to `write_calibrated_output`, which
/// takes the channel count as a parameter for exactly that reason.
pub struct CalibratedFrame {
    /// Frame width, in pixels.
    pub width: usize,
    /// Frame height, in pixels.
    pub height: usize,
    /// The calibrated samples, normalized (DN over the scale divisor, negatives kept), row-major, `width * height`
    /// long.
    pub data: Vec<f32>,
}

/// One master's plane preloaded through `BandSource`, with the geometry it
/// was decoded at (fix round 1, ruled-in item 4). Carrying `width`/`height`
/// alongside `data` restores the strictness `BandSource::open` has on a
/// direct read — a transposed master (same total sample count, wrong shape)
/// now fails the geometry check below instead of passing a length-only
/// comparison.
pub struct PreloadedPlane {
    /// The master's decoded plane, in DN, row-major, `width * height` long; shared so every frame of a group reads
    /// one copy.
    pub data: Arc<Vec<f32>>,
    /// Width of the decoded plane, in pixels.
    pub width: usize,
    /// Height of the decoded plane, in pixels.
    pub height: usize,
}

/// Master planes already decoded through `BandSource` — the same decode the
/// engine would run itself, so the samples are bit-identical (perf tier 1
/// Task 5). Keyed by the master's path; the engine uses an entry only when
/// the path matches its own `dark_path`/`bias_path`/`flat_path` AND its
/// `(width, height)` matches the light's own, and reads the file itself
/// otherwise.
pub struct PreloadedMasters {
    /// The decoded master planes, keyed by the master's file path.
    pub planes: HashMap<PathBuf, PreloadedPlane>,
}

/// Decode one master file's single plane through the same `BandSource` /
/// `BandPlanes` path the engine uses for a light's own subtrahend/flat reads
/// (perf tier 1 Task 5) — so a caller that preloads a group's shared dark and
/// flat once produces bit-identical samples to the engine reading them itself
/// per frame. Always the floor budget ([`MIN_BUDGET_BYTES`]): a single master
/// plane is 1-2 bands regardless, same reasoning as `calibrate_light`.
///
/// Cancellable (fix round 1, ruled-in item 5): `cancel` reaches both
/// `BandSource::open_with_cancel` (a probe abandoned mid-file) and
/// `read_band_with_progress` (a read abandoned mid-band, progress itself a
/// no-op — nothing in this path drives a progress bar), so a run cancelled
/// while preloading a large master's planes stops within one file rather
/// than running every remaining band to completion first.
pub fn preload_master_plane(
    path: &Path,
    scratch_dir: &Path,
    cancel: &AtomicBool,
) -> Result<PreloadedPlane, IntegrationError> {
    let src = BandSource::open_with_cancel(&[path.to_path_buf()], scratch_dir, 1, cancel)?;
    let (w, h) = (src.width(), src.height());
    let mut planes = BandPlanes::new(&src);
    let mut out = vec![0f32; w * h];
    let band_rows = src.band_rows_for_budget(MIN_BUDGET_BYTES).min(h).max(1);
    let mut y = 0;
    while y < h {
        let rows = band_rows.min(h - y);
        src.read_band_with_progress(y, rows, &mut planes, 1, &|_| {}, cancel)?;
        planes.decode_frame_into(0, &mut out[y * w..(y + rows) * w]);
        y += rows;
    }
    Ok(PreloadedPlane { data: Arc::new(out), width: w, height: h })
}

/// The formula pass on its own — everything `calibrate_light` does EXCEPT the
/// file write and the hash, so a caller can put its own pixel stages between
/// the two halves.
///
/// The returned outcome is complete but for [`LightCalOutcome::output_hash`],
/// which stays empty until `write_calibrated_output` produces it. Nothing is
/// written to `inputs.output_path` here; that path is the write phase's
/// business, and a caller is free to write the result somewhere else entirely.
///
/// Cancellation is cooperative, checked once per band before any pixel work.
pub fn calibrate_light_compute(
    inputs: &LightCalInputs,
    cancel: &AtomicBool,
) -> Result<(CalibratedFrame, LightCalOutcome), IntegrationError> {
    calibrate_light_compute_with(inputs, None, cancel)
}

/// [`calibrate_light_compute`] with a group's shared master dark/bias and
/// flat already decoded (perf tier 1 Task 5): when `preloaded` holds an
/// entry whose path matches `inputs.dark_path`/`bias_path` or
/// `inputs.flat_path`, the engine reads that plane from RAM instead of
/// opening a second `BandSource` over the file — the light itself is always
/// read through `BandSource`, preload or not. `preloaded: None` (what
/// [`calibrate_light_compute`] passes) is the original per-frame-reads-
/// everything behavior, byte-for-byte.
pub fn calibrate_light_compute_with(
    inputs: &LightCalInputs,
    preloaded: Option<&PreloadedMasters>,
    cancel: &AtomicBool,
) -> Result<(CalibratedFrame, LightCalOutcome), IntegrationError> {
    // Same reasoning as `calibrate_light`: one frame, 1-2 bands regardless of
    // budget, so the floor is not the bottleneck here (spec §8).
    calibrate_light_compute_budgeted(inputs, preloaded, cancel, MIN_BUDGET_BYTES, &NoFlatCards)
}

/// [`calibrate_light_compute_with`] with the reader of the master flat's stamped constants as a parameter: the
/// flat-normalization divisor is resolved through `cards` ([`resolve_flat_norm_divisor_with`]). The plain names
/// pass [`NoFlatCards`]; an application that stamps constants into its master flats passes its own reader.
pub fn calibrate_light_compute_with_cards(
    inputs: &LightCalInputs,
    preloaded: Option<&PreloadedMasters>,
    cards: &dyn FlatCardReader,
    cancel: &AtomicBool,
) -> Result<(CalibratedFrame, LightCalOutcome), IntegrationError> {
    calibrate_light_compute_budgeted(inputs, preloaded, cancel, MIN_BUDGET_BYTES, cards)
}

/// Calibrates one light frame like [`calibrate_light_compute_with_cards`], with the band-memory budget as a
/// parameter (`band_budget_bytes`, in bytes), so a caller can force a multi-band run on a small fixture.
///
/// Runs on the calling thread, takes no pool and spawns no thread (its band reads run at concurrency 1). A source
/// that must be decoded first (XISF, or a FITS with more than one plane) is decoded with rayon iterators on the
/// caller's current rayon pool, so it is safe to call from inside that pool.
pub fn calibrate_light_compute_budgeted(
    inputs: &LightCalInputs,
    preloaded: Option<&PreloadedMasters>,
    cancel: &AtomicBool,
    band_budget_bytes: usize,
    cards: &dyn FlatCardReader,
) -> Result<(CalibratedFrame, LightCalOutcome), IntegrationError> {
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

    // Flat-normalization divisor actually applied: one global constant, or one
    // per CFA channel (spec §2 + the CFA hardening cycle). Resolved before the
    // read so a missing flat or an unusable constant fails fast, before any
    // output work.
    let divisor = match (&inputs.flat_path, inputs.flat_norm) {
        (Some(flat), true) => resolve_flat_norm_divisor_with(
            flat,
            &inputs.scratch_dir,
            inputs.flat_norm_mode,
            &inputs.params,
            inputs.cfa_geometry,
            cards,
        )?,
        _ => FlatNormDivisor::Global(1.0),
    };
    let flat_norm_divisor = divisor.global_value();
    // Hoisted out of the pixel loop: the per-channel arm needs the pixel's
    // (x, y), the global arm never does.
    let per_channel = match divisor {
        FlatNormDivisor::Global(_) => None,
        FlatNormDivisor::PerChannel { geom, k } => Some((geom, k)),
    };

    // Output pedestal (spec §2): DN added AFTER the scale divide. Precomputed in
    // output units once; the add is skipped entirely when the pedestal is 0.
    let add_pedestal = inputs.params.pedestal_dn != 0.0;
    let pedestal_offset = inputs.params.pedestal_dn / inputs.scale_divisor;

    // Perf tier 1 Task 5: when a shared preload already holds this frame's
    // subtrahend and/or flat plane (a stacking run decodes a group's master
    // dark/bias and flat once and hands them to every frame), read those
    // samples from RAM instead of opening a second `BandSource` over the
    // file. Resolved BEFORE `paths` below, so a preloaded path is never
    // pushed onto the light's own `BandSource` read.
    let pre_sub: Option<&PreloadedPlane> =
        subtrahend.and_then(|p| preloaded.and_then(|m| m.planes.get(p)));
    let pre_flat: Option<&PreloadedPlane> =
        inputs.flat_path.as_ref().and_then(|p| preloaded.and_then(|m| m.planes.get(p)));

    // One BandSource over the light plus whichever of subtrahend/flat is NOT
    // already preloaded, remembering each frame's index. BandSource::open
    // validates geometry itself and rejects mixed dimensions as
    // IntegrationError::BadInput — no separate pre-check needed for the
    // files it actually opens.
    let mut paths: Vec<PathBuf> = vec![inputs.light_path.clone()];
    let sub_idx = subtrahend.and_then(|p| {
        if pre_sub.is_some() {
            None
        } else {
            paths.push(p.clone());
            Some(paths.len() - 1)
        }
    });
    let flat_idx = inputs.flat_path.as_ref().and_then(|p| {
        if pre_flat.is_some() {
            None
        } else {
            paths.push(p.clone());
            Some(paths.len() - 1)
        }
    });

    // No `IoPolicy` at this call site (spec §8 keeps light calibration on the
    // floor budget) and at most 3 frames (light + subtrahend + flat) — there
    // is nothing to parallelize here, and inventing a policy for a 1-3 frame
    // read would widen this cycle into the calibrated-export path.
    let src = BandSource::open(&paths, &inputs.scratch_dir, 1)?;
    let (w, h) = (src.width(), src.height());

    // A preloaded plane must match the light's own geometry — the same
    // failure class `BandSource::open` raises for a mixed-dimension read,
    // just checked against RAM instead of a second file open. Comparing
    // `(width, height)` rather than just the sample count (fix round 1,
    // ruled-in item 4) catches a transposed master too — same total
    // samples, wrong shape.
    for (path, plane) in [(subtrahend, pre_sub), (inputs.flat_path.as_ref(), pre_flat)] {
        if let (Some(path), Some(plane)) = (path, plane) {
            if (plane.width, plane.height) != (w, h) {
                return Err(IntegrationError::BadInput(format!(
                    "preloaded master geometry mismatch: {} is {}x{}, the light is {w}x{h}",
                    path.display(),
                    plane.width,
                    plane.height
                )));
            }
        }
    }

    let band_rows = src.band_rows_for_budget(band_budget_bytes).min(h);
    let mut planes = BandPlanes::new(&src);
    let mut out = vec![0f32; w * h];

    // Dead/negative flat pixels that hit FLAT_DENOM_FLOOR. The band loop is
    // serial, so a plain counter is correct.
    let mut floored_flat_pixels: u64 = 0;
    // Pixels at or below zero right after the subtraction. A counter only: it
    // reads `v`, never writes it, so the output bytes do not depend on it.
    let subtracted = pre_sub.is_some() || sub_idx.is_some();
    let mut non_positive_pixels: u64 = 0;

    let mut y = 0;
    while y < h {
        if cancel.load(Ordering::Relaxed) {
            return Err(IntegrationError::Cancelled);
        }
        let rows = band_rows.min(h - y);
        src.read_band(y, rows, &mut planes, 1)?;
        let out_band = &mut out[y * w..(y + rows) * w];
        for (idx, out_px) in out_band.iter_mut().enumerate() {
            // f64 throughout, cast once at the end — negatives and division are
            // preserved with no clamping or pedestal (spec §2).
            let mut v = planes.sample(0, idx) as f64;
            // `full` is the pixel's index into a preloaded plane, which is
            // sized to the WHOLE frame (`w * h`) rather than one band — `idx`
            // alone is band-local and would read the wrong row past the
            // first band.
            let full = y * w + idx;
            match (pre_sub, sub_idx) {
                (Some(sub), _) => v -= sub.data[full] as f64,
                (None, Some(si)) => v -= planes.sample(si, idx) as f64,
                (None, None) => {}
            }
            if subtracted && v <= 0.0 {
                non_positive_pixels += 1;
            }
            let flat_sample = match (pre_flat, flat_idx) {
                (Some(f), _) => Some(f.data[full] as f64),
                (None, Some(fi)) => Some(planes.sample(fi, idx) as f64),
                (None, None) => None,
            };
            if let Some(flat_sample) = flat_sample {
                // The constant this pixel divides by. In per-channel mode it is
                // the constant of the pixel's own mosaic colour, which needs the
                // pixel's position in the FRAME — `idx` is band-local, so the
                // row is the band's start plus the band-local row. A band-local
                // row would shift the CFA phase by one on every odd-height band
                // and swap R with B from the second band onward.
                let k = match per_channel {
                    None => flat_norm_divisor,
                    Some((geom, k)) => {
                        let (x, gy) = (idx % w, y + idx / w);
                        k[cfa_channel_at(x, gy, geom).idx()]
                    }
                };
                // A dead (0.0), negative, or non-finite flat pixel would make
                // this division Inf/NaN or flip the light's sign. Floor the
                // denominator instead and count the hit — the frame stays
                // usable and the count is warned about below. A master flat
                // built here writes an all-undefined pixel as exactly 0.0, so
                // the zero case is reachable, not hypothetical.
                let denom = flat_sample / k;
                if denom.is_finite() && denom >= FLAT_DENOM_FLOOR {
                    v /= denom;
                } else {
                    v /= FLAT_DENOM_FLOOR;
                    floored_flat_pixels += 1;
                }
            }
            v /= inputs.scale_divisor;
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
            // Frame pixel count, so `count` reads as a fraction rather than a
            // bare number (a few hundred hits mean something different on a
            // 1 MP frame than on a 60 MP one).
            total = (w * h) as u64,
            "flat denominator floored (dead/negative flat pixels)"
        );
    }

    let calstat = if has_flat {
        format!("{calstat_base}F")
    } else {
        calstat_base.to_string()
    };

    Ok((
        CalibratedFrame { width: w, height: h, data: out },
        LightCalOutcome {
            calstat,
            flat_norm_divisor,
            flat_channel_divisors: divisor.channel_values(),
            cfa_scaling_applied: divisor.is_per_channel(),
            // Empty until the write phase hashes the file it produced.
            output_hash: String::new(),
            floored_flat_pixels,
            non_positive_pixels,
            pixels: if subtracted { (w * h) as u64 } else { 0 },
        },
    ))
}

/// Resolve how `flat_path` normalizes: one constant for the whole frame, or one
/// per CFA channel. THE single decision point — the engine and the orchestration
/// layer (which resolves again to stamp the header) both call this, so the
/// written cards and the applied math cannot drift.
///
/// Per-channel scaling applies only when ALL of these hold; any miss falls back
/// to [`FlatNormDivisor::Global`] for the WHOLE frame (never mixed-mode, where
/// two channels would be per-channel-scaled and the third not):
///
/// 1. [`LightCalParams::cfa_flat_scaling`] is on (default),
/// 2. the LIGHT declares a mosaic phase (`cfa` is `Some` — mono lights have no
///    channels to separate),
/// 3. the mode is [`FlatNormMode::CentralThird`] — `TrimmedMeanWhole` is a
///    whole-frame tool-parity statistic and is left alone (`debug!`),
/// 4. the three constants hold up as divisors, resolved in this order:
///    a. the master flat's stamped per-channel constants (R, G and B) — all
///       three finite and > 0, AND the flat's own declared mosaic phase (the
///       phase they were measured under) agreeing with the light's, else
///    b. recomputed from the flat's own pixels over the same central-third
///       window the global constant uses, under the LIGHT's geometry (the
///       imported-flat path, and the phase-disagreement path), else
///    c. degenerate → `warn!` and fall back to global.
///
/// Reading the plane at most once is deliberate: this runs per LIGHT frame, and
/// a master flat can be hundreds of megabytes. The card path reads no pixels at
/// all, and the recompute path reads the plane exactly once — the same cost the
/// global constant already paid for a card-less flat. One case does get slower
/// than before: a master flat built BEFORE the per-channel constants were stamped
/// carries only the global constant, so a CFA run recomputes from its
/// pixels for every light instead of reading a stamped value. Rebuilding that master
/// stamps the constants and the reads go away; a master built since always has them.
pub fn resolve_flat_norm_divisor(
    flat_path: &Path,
    scratch_dir: &Path,
    mode: FlatNormMode,
    params: &LightCalParams,
    cfa: Option<CfaGeometry>,
) -> Result<FlatNormDivisor, IntegrationError> {
    resolve_flat_norm_divisor_with(flat_path, scratch_dir, mode, params, cfa, &NoFlatCards)
}

/// [`resolve_flat_norm_divisor`] with the reader of the flat's stamped constants as a parameter: `cards` is asked
/// for the per-channel constants (step 4a above) and, through [`flat_norm_constant_with`], for the global one.
pub fn resolve_flat_norm_divisor_with(
    flat_path: &Path,
    scratch_dir: &Path,
    mode: FlatNormMode,
    params: &LightCalParams,
    cfa: Option<CfaGeometry>,
    cards: &dyn FlatCardReader,
) -> Result<FlatNormDivisor, IntegrationError> {
    let global = || {
        flat_norm_constant_with(flat_path, scratch_dir, mode, params.trim_fraction, cards)
            .map(FlatNormDivisor::Global)
    };

    if !params.cfa_flat_scaling {
        return global();
    }
    let Some(geom) = cfa else {
        // Mono: nothing to separate. Not worth a log line — it is the majority
        // of frames and says nothing about this run.
        return global();
    };
    if mode == FlatNormMode::TrimmedMeanWhole {
        tracing::debug!(
            path = %flat_path.display(),
            "per-channel flat scaling ignored in trimmed-mean-whole mode (whole-frame statistic)"
        );
        return global();
    }

    let k = match cards.channel_norms(flat_path, geom) {
        Some(k) => {
            tracing::debug!(path = %flat_path.display(), value = ?k, "per-channel flat normalization from ATH_FNR/G/B cards");
            k
        }
        None => {
            let (w, h, data) = read_full_flat_plane(flat_path, scratch_dir)?;
            let k = central_third_channel_means(&data, w, h, geom);
            tracing::debug!(path = %flat_path.display(), value = ?k, "per-channel flat normalization recomputed (cards absent)");
            k
        }
    };
    if k.iter().all(|v| v.is_finite() && *v > 0.0) {
        Ok(FlatNormDivisor::PerChannel { geom, k })
    } else {
        // A channel with no usable constant cannot be divided by it, and
        // scaling only the OTHER channels would tint the frame worse than not
        // scaling at all. Fall back whole-frame, loudly.
        tracing::warn!(
            path = %flat_path.display(),
            value = ?k,
            "per-channel flat constants degenerate — falling back to global normalization"
        );
        global()
    }
}

/// The flat-normalization divisor for `flat_path` under `mode` (spec §2):
///
/// - [`FlatNormMode::CentralThird`] (default): read the global normalization
///   constant stamped on the master flat, or — for a flat imported
///   without one — recompute the central-third mean on the fly.
/// - [`FlatNormMode::TrimmedMeanWhole`] (whole-frame parity statistic): a
///   two-sided trimmed mean over the WHOLE frame (`trim_fraction` per tail, default
///   [`TRIMMED_MEAN_TAIL_FRACTION`]). ALWAYS computed from the flat file; a stamped
///   constant is ignored.
///
/// `trim_fraction` is only consulted in `TrimmedMeanWhole` mode (the
/// central-third path never trims); callers in central-third mode may pass
/// [`TRIMMED_MEAN_TAIL_FRACTION`].
///
/// Whatever the mode, the resolved constant must be finite and strictly
/// positive to serve as a divisor — anything else is
/// [`IntegrationError::BadInput`]. A stamped constant that fails that test
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
    flat_norm_constant_with(flat_path, scratch_dir, mode, trim_fraction, &NoFlatCards)
}

/// [`flat_norm_constant`] with the reader of the flat's stamped constant as a parameter: in
/// [`FlatNormMode::CentralThird`] `cards` is asked for the global constant before the pixels are read.
pub fn flat_norm_constant_with(
    flat_path: &Path,
    scratch_dir: &Path,
    mode: FlatNormMode,
    trim_fraction: f64,
    cards: &dyn FlatCardReader,
) -> Result<f64, IntegrationError> {
    let n = match mode {
        FlatNormMode::CentralThird => {
            if let Some(n) = cards.fnrm(flat_path) {
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
        FlatNormMode::TrimmedMeanWhole => {
            // Whole-frame parity: the card's meaning (central-third) does not
            // match this statistic, so it is deliberately ignored — always
            // computed from the flat's pixels.
            let (_w, _h, data) = read_full_flat_plane(flat_path, scratch_dir)?;
            let mean = two_sided_trimmed_mean(&data, trim_fraction);
            tracing::debug!(path = %flat_path.display(), trimmed_mean = mean, trim_fraction, "flat normalization from full-frame trimmed mean");
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
    // One frame, no `IoPolicy` at this call site — nothing to parallelize.
    let src = BandSource::open(&[flat_path.to_path_buf()], scratch_dir, 1)?;
    let (w, h) = (src.width(), src.height());
    // One flat plane, one frame — already 1-2 bands at the floor, so the
    // machine-resolved budget would not change the band count here (spec §8).
    let band_rows = src.band_rows_for_budget(MIN_BUDGET_BYTES).min(h);
    let mut data = vec![0f32; w * h];
    let mut planes = BandPlanes::new(&src);
    let mut y = 0;
    while y < h {
        let rows = band_rows.min(h - y);
        src.read_band(y, rows, &mut planes, 1)?;
        planes.decode_frame_into(0, &mut data[y * w..(y + rows) * w]);
        y += rows;
    }
    Ok((w, h, data))
}

/// The flat's darkest corner over its normalization divisor (spec
/// 2026-10-04 §5.3, L-18): the median of each `c × c` corner patch,
/// `c = max(16, round(0.05·min(W, H)))`, the smallest of the four, divided
/// by `divisor` (the generator's `FlatNormDivisor::global_value`). Reads
/// only the top and bottom `c` rows, band by band.
pub fn flat_corner_ratio(
    flat_path: &Path,
    scratch_dir: &Path,
    divisor: f64,
) -> Result<f64, IntegrationError> {
    if !(divisor.is_finite() && divisor > 0.0) {
        return Err(IntegrationError::BadInput(format!(
            "flat corner divisor {divisor} unusable ({})",
            flat_path.display()
        )));
    }
    let src = BandSource::open(&[flat_path.to_path_buf()], scratch_dir, 1)?;
    let (w, h) = (src.width(), src.height());
    let c = 16usize
        .max((0.05 * w.min(h) as f64).round() as usize)
        .min(w.min(h) / 2)
        .max(1);
    let band_rows = src.band_rows_for_budget(MIN_BUDGET_BYTES).max(1);
    let mut planes = BandPlanes::new(&src);
    let mut read_rows = |y0: usize| -> Result<Vec<f32>, IntegrationError> {
        let mut out = vec![0f32; c * w];
        let mut y = 0;
        while y < c {
            let rows = band_rows.min(c - y);
            src.read_band(y0 + y, rows, &mut planes, 1)?;
            planes.decode_frame_into(0, &mut out[y * w..(y + rows) * w]);
            y += rows;
        }
        Ok(out)
    };
    let (top, bottom) = (read_rows(0)?, read_rows(h - c)?);
    let patch = |rows: &[f32], x0: usize| -> f32 {
        let mut v: Vec<f32> = (0..c)
            .flat_map(|r| rows[r * w + x0..r * w + x0 + c].iter().copied())
            .filter(|p| p.is_finite())
            .collect();
        if v.is_empty() {
            f32::NAN
        } else {
            crate::integration::stats::median_in_place(&mut v)
        }
    };
    let darkest = [
        patch(&top, 0),
        patch(&top, w - c),
        patch(&bottom, 0),
        patch(&bottom, w - c),
    ]
    .into_iter()
    .filter(|m| m.is_finite())
    .fold(f32::INFINITY, f32::min);
    if !darkest.is_finite() || darkest <= 0.0 {
        return Err(IntegrationError::BadInput(format!(
            "flat corners unreadable or not positive ({})",
            flat_path.display()
        )));
    }
    Ok(darkest as f64 / divisor)
}

/// Two-sided trimmed mean over the whole plane, discarding exactly
/// `trim_fraction` of the pixels from EACH tail (a common two-sided clipping
/// convention for flat normalization, spec §2; `trim_fraction` defaults to
/// [`TRIMMED_MEAN_TAIL_FRACTION`], the advanced param exposes it). Sorts a copy (total
/// order, NaN-tolerant) and averages the surviving middle in f64:
/// `lo = floor(n·f)`, `hi = n − floor(n·f)`, mean over `sorted[lo..hi]`.
///
/// Degenerate guards: an empty plane returns `1.0` (a harmless divide-by later),
/// and a frame so small that `lo >= hi` falls back to the full-frame mean rather
/// than averaging an empty slice.
fn two_sided_trimmed_mean(data: &[f32], trim_fraction: f64) -> f64 {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::write_fits_f32;

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

    /// Spec §5.3: the smallest of the four corner-patch medians over the
    /// normalization divisor; c = max(16, round(0.05·min(W, H))).
    #[test]
    fn flat_corner_ratio_reads_the_darkest_corner_over_the_divisor() {
        let dir = tempfile::tempdir().unwrap();
        // 400 x 300: c = max(16, 15) = 16; the 20 x 20 corners hold the patches.
        let flat = write_fill(
            dir.path(),
            "flat.fits",
            400,
            300,
            |x, y| {
                let right = x >= 380;
                let bottom = y >= 280;
                if x < 20 && y < 20 {
                    0.5
                } else if (x < 20 || right) && (y < 20 || bottom) {
                    0.7
                } else {
                    1.0
                }
            },
            &[],
        );
        let r = flat_corner_ratio(&flat, dir.path(), 1.0).unwrap();
        assert!((r - 0.5).abs() < 1e-6, "ratio {r}");
        let r = flat_corner_ratio(&flat, dir.path(), 0.8).unwrap();
        assert!((r - 0.625).abs() < 1e-6, "ratio {r}");
        assert!(flat_corner_ratio(&flat, dir.path(), 0.0).is_err());
        // A zero corner and an all-NaN corner set are refused, not reported.
        let zero = write_fill(
            dir.path(),
            "zero.fits",
            400,
            300,
            |x, y| if x < 20 && y < 20 { 0.0 } else { 1.0 },
            &[],
        );
        assert!(flat_corner_ratio(&zero, dir.path(), 1.0).is_err());
        let nan = write_fill(
            dir.path(),
            "nan.fits",
            400,
            300,
            |x, y| {
                if (x < 20 || x >= 380) && (y < 20 || y >= 280) {
                    f32::NAN
                } else {
                    1.0
                }
            },
            &[],
        );
        assert!(flat_corner_ratio(&nan, dir.path(), 1.0).is_err());
    }

    #[test]
    fn two_sided_trimmed_mean_exact() {
        // 1000 distinct values 0..=999 → trim floor(1000*0.05)=50 from each
        // tail → mean of sorted[50..950] = mean(50..=949) = (50+949)/2 = 499.5.
        let vals: Vec<f32> = (0..1000).map(|i| i as f32).collect();
        let m = two_sided_trimmed_mean(&vals, TRIMMED_MEAN_TAIL_FRACTION);
        assert!((m - 499.5).abs() < 1e-9, "trimmed mean {m}, want 499.5");

        // Order-independent: shuffling the input must not change the statistic
        // (the fn sorts internally).
        let mut rev: Vec<f32> = vals.clone();
        rev.reverse();
        assert!((two_sided_trimmed_mean(&rev, TRIMMED_MEAN_TAIL_FRACTION) - 499.5).abs() < 1e-9);
    }

    #[test]
    fn trim_fraction_changes_divisor() {
        // A right-skewed distribution (quadratic ramp) so a wider trim actually
        // changes the two-sided trimmed mean — a symmetric set would give the
        // same statistic at any fraction.
        let data: Vec<f32> = (0..1000).map(|i| (i * i) as f32).collect();
        let m05 = two_sided_trimmed_mean(&data, 0.05);
        let m10 = two_sided_trimmed_mean(&data, 0.10);
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
        let got = flat_norm_constant(&flat, dir.path(), FlatNormMode::TrimmedMeanWhole, 0.10).unwrap();
        assert!((got - expected).abs() < 1e-2, "engine trim-0.10 path {got} vs formula {expected}");
    }

    #[test]
    fn two_sided_trimmed_mean_real_shape_matches_formula() {
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
        let expected = two_sided_trimmed_mean(&data, TRIMMED_MEAN_TAIL_FRACTION);
        // Sanity: the trimmed mean sits strictly inside the value range.
        let (mn, mx) = data.iter().fold((f32::MAX, f32::MIN), |(a, b), &v| (a.min(v), b.max(v)));
        assert!((mn as f64) < expected && expected < (mx as f64), "trimmed mean {expected} outside ({mn},{mx})");

        let got = flat_norm_constant(&flat, dir.path(), FlatNormMode::TrimmedMeanWhole, TRIMMED_MEAN_TAIL_FRACTION).unwrap();
        assert!((got - expected).abs() < 1e-3, "engine path {got} vs direct formula {expected}");
    }

    #[test]
    fn non_positive_normalization_constant_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (9usize, 9usize);
        // An all-zero flat: no card, and the recomputed mean is 0.0 — dividing
        // by it is meaningless, so the run fails loudly instead of silently
        // producing Inf.
        let flat = write_plane(dir.path(), "flat_dead.fits", w, h, 0.0, &[]);
        let r = flat_norm_constant(&flat, dir.path(), FlatNormMode::CentralThird, TRIMMED_MEAN_TAIL_FRACTION);
        assert!(matches!(r, Err(IntegrationError::BadInput(_))), "expected BadInput, got {r:?}");
    }
}

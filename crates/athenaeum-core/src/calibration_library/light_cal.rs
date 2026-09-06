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
//! output lands in ~[0,1] whatever the light was stored as; and
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
use crate::fits_writer::keywords::Bayer;
use crate::fits_writer::{write_fits_f32, Card};
use crate::integration::band_budget::MIN_BUDGET_BYTES;
use crate::integration::banded::{BandPlanes, BandSource};
use crate::integration::cfa::{cfa_channel_at, central_third_channel_means, CfaGeometry};
use crate::integration::engine::central_third_mean;
use crate::integration::IntegrationError;
use crate::models::FileFormat;

// The shared light-calibration data types + consts (LIGHT_CAL_ENGINE_VERSION,
// PI_TRIM_FRACTION, FlatNormMode, BiasFallback, LightCalParams) live in
// `crate::models` so ungated consumers (scanner, export) compile with
// `--no-default-features`. They are re-exported here so
// this engine and its render-gated callers keep using the `light_cal::` paths.
pub use crate::models::{
    BiasFallback, FlatNormMode, LightCalParams, LIGHT_CAL_ENGINE_VERSION, PI_TRIM_FRACTION,
};

/// Divisor that normalizes a 16-bit source's counts to roughly `[0, 1]`
/// (spec §2; stamped into the output header as `ATH_CSCL`). Also the fallback
/// for a source whose bit depth cannot be read — see
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
/// denominator is `flat / ATH_FNRM` (values around 1.0, so the floor only
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
    /// `trim_fraction` (feeds `pixinsightTrimmed` normalization) and
    /// `pedestal_dn` (added after the scale divide); `bias_fallback` is enforced
    /// by the orchestration layer BEFORE the engine runs, so the engine is
    /// agnostic to it.
    pub params: LightCalParams,
    /// Divisor applied AFTER the calibration arithmetic to bring the output to
    /// ~[0,1] — the light source's own bit-depth maximum, resolved by the
    /// caller via [`scale_divisor_for_bitpix`] (the engine never re-reads the
    /// source header for it) and stamped as `ATH_CSCL` by the header builder,
    /// so the written card and the applied value are the same number.
    pub scale_divisor: f64,
    pub output_path: PathBuf,
    pub cards: Vec<Card>,
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
    Global(f64),
    PerChannel { geom: CfaGeometry, k: [f64; 3] },
}

impl FlatNormDivisor {
    /// The single number that describes this divisor for the tracking row and
    /// the `ATH_CFNM` card: the constant itself in `Global` mode, and in
    /// `PerChannel` mode the constants blended **by their share of the mosaic** —
    /// `(R + 2G + B) / 4`, since a Bayer cell is half green. That weighting is
    /// what makes the number continuous with the global constant it stands in
    /// for: the global central-third mean averages the window's pixels, which
    /// are green half the time, so an unweighted `(R+G+B)/3` would sit somewhere
    /// the global path never produces. Per-channel runs also stamp the three
    /// constants individually (`ATH_CFNR`/`ATH_CFNG`/`ATH_CFNB`), so this
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

    pub fn is_per_channel(self) -> bool {
        matches!(self, FlatNormDivisor::PerChannel { .. })
    }
}

/// What the engine actually applied, for the tracking row + progress summary.
#[derive(Debug, Clone, PartialEq)]
pub struct LightCalOutcome {
    /// Applied-state flags (`"BDF"`, `"BF"`, `"BD"`, `"B"`, `"F"`).
    pub calstat: String,
    /// Divisor actually used for flat normalization: the `ATH_FNRM` value when
    /// normalization was on, else `1.0` (also `1.0` when no flat was applied).
    /// In per-channel mode this is the mosaic-weighted blend of the three
    /// channel constants ([`FlatNormDivisor::global_value`]) — the
    /// global-equivalent number, kept so the row column and the `ATH_CFNM` card
    /// mean the same thing in both modes.
    pub flat_norm_divisor: f64,
    /// The `[R, G, B]` constants when the flat was normalized per CFA channel,
    /// else `None`. `Some` implies [`LightCalOutcome::cfa_scaling_applied`].
    pub flat_channel_divisors: Option<[f64; 3]>,
    /// Whether per-channel scaling was ACTUALLY applied — not merely requested.
    /// `false` for a mono light, a `pixinsightTrimmed` run, a flat whose channel
    /// constants came back degenerate, or the toggle simply being off. This is
    /// what the tracking row records.
    pub cfa_scaling_applied: bool,
    /// xxh3 of the written output file.
    pub output_hash: String,
    /// How many pixels hit [`FLAT_DENOM_FLOOR`] because the flat's value there
    /// was dead, negative, or non-finite. Reported/logged only — a floored
    /// pixel is a warning, never a frame failure.
    pub floored_flat_pixels: u64,
}

/// One calibrated image plane held in RAM, handed from the compute phase to the
/// write phase.
///
/// The split exists so intermediate pixel stages can run between the formula
/// and the file write without either phase knowing about them. `data` is
/// row-major and `width * height` long — a stage that turns one plane into
/// several hands its own buffer straight to [`write_calibrated_output`], which
/// takes the channel count as a parameter for exactly that reason.
pub struct CalibratedFrame {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
}

/// Calibrate one LIGHT frame and write the result to `inputs.output_path`.
///
/// Cancellation is cooperative: checked once per band before any pixel work,
/// so a cancel that lands before the write leaves no output file behind.
pub fn calibrate_light(
    inputs: &LightCalInputs,
    cancel: &AtomicBool,
) -> Result<LightCalOutcome, IntegrationError> {
    // A single light is one frame — already 1-2 bands at the floor budget, so
    // the machine-resolved budget (`integration::band_budget`) buys nothing
    // here; kept at the floor deliberately (spec §8).
    calibrate_light_inner(inputs, cancel, MIN_BUDGET_BYTES)
}

/// The formula pass on its own — everything [`calibrate_light`] does EXCEPT the
/// file write and the hash, so a caller can put its own pixel stages between
/// the two halves.
///
/// The returned outcome is complete but for [`LightCalOutcome::output_hash`],
/// which stays empty until [`write_calibrated_output`] produces it. Nothing is
/// written to `inputs.output_path` here; that path is the write phase's
/// business, and a caller is free to write the result somewhere else entirely.
///
/// Cancellation is cooperative, checked once per band before any pixel work.
pub fn calibrate_light_compute(
    inputs: &LightCalInputs,
    cancel: &AtomicBool,
) -> Result<(CalibratedFrame, LightCalOutcome), IntegrationError> {
    // Same reasoning as `calibrate_light`: one frame, 1-2 bands regardless of
    // budget, so the floor is not the bottleneck here (spec §8).
    calibrate_light_compute_inner(inputs, cancel, MIN_BUDGET_BYTES)
}

/// Write a calibrated plane to `path` and return the xxh3 of the written file.
///
/// The write is atomic — [`write_fits_f32`] stages a sibling temp file and
/// renames it into place, so a failed write never truncates a good file already
/// sitting at `path`. `channels` is a parameter rather than the constant `1`
/// because a debayered frame goes out through this same door.
pub fn write_calibrated_output(
    path: &Path,
    width: usize,
    height: usize,
    channels: usize,
    data: &[f32],
    cards: &[Card],
) -> Result<String, IntegrationError> {
    write_fits_f32(path, width, height, channels, data, cards)
        .map_err(|e| io_err(format!("writing {}: {e}", path.display())))?;
    compute_xxhash(path).map_err(|e| io_err(format!("hashing {}: {e:#}", path.display())))
}

/// [`calibrate_light`] with the band-memory budget as a parameter, so tests can
/// force a multi-band run on a small fixture (the integration engine's
/// `integrate_flat_inner` takes its band size the same way).
fn calibrate_light_inner(
    inputs: &LightCalInputs,
    cancel: &AtomicBool,
    band_budget_bytes: usize,
) -> Result<LightCalOutcome, IntegrationError> {
    let (frame, mut outcome) = calibrate_light_compute_inner(inputs, cancel, band_budget_bytes)?;
    outcome.output_hash = write_calibrated_output(
        &inputs.output_path,
        frame.width,
        frame.height,
        1,
        &frame.data,
        &inputs.cards,
    )?;

    tracing::debug!(
        src = %inputs.light_path.display(),
        dest = %inputs.output_path.display(),
        calstat = %outcome.calstat,
        flat_norm_divisor = outcome.flat_norm_divisor,
        cfa_scaling_applied = outcome.cfa_scaling_applied,
        width = frame.width,
        height = frame.height,
        floored_flat_pixels = outcome.floored_flat_pixels,
        "light calibrated"
    );

    Ok(outcome)
}

/// [`calibrate_light_compute`] with the band-memory budget as a parameter, for
/// the same reason [`calibrate_light_inner`] takes one.
fn calibrate_light_compute_inner(
    inputs: &LightCalInputs,
    cancel: &AtomicBool,
    band_budget_bytes: usize,
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
        (Some(flat), true) => resolve_flat_norm_divisor(
            flat,
            &inputs.scratch_dir,
            inputs.flat_norm_mode,
            &inputs.params,
            inputs.cfa_geometry,
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
    let (w, h) = (src.width(), src.height());
    let band_rows = src.band_rows_for_budget(band_budget_bytes).min(h);
    let mut planes = BandPlanes::new(&src);
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
        src.read_band(y, rows, &mut planes)?;
        let out_band = &mut out[y * w..(y + rows) * w];
        for (idx, out_px) in out_band.iter_mut().enumerate() {
            // f64 throughout, cast once at the end — negatives and division are
            // preserved with no clamping or pedestal (spec §2).
            let mut v = planes.sample(0, idx) as f64;
            if let Some(si) = sub_idx {
                v -= planes.sample(si, idx) as f64;
            }
            if let Some(fi) = flat_idx {
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
                let denom = planes.sample(fi, idx) as f64 / k;
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
/// 3. the mode is [`FlatNormMode::CentralThird`] — `pixinsightTrimmed` is a
///    whole-frame tool-parity statistic and is left alone (`debug!`),
/// 4. the three constants hold up as divisors, resolved in this order:
///    a. the master flat's stamped `ATH_FNR`/`ATH_FNG`/`ATH_FNB` cards — all
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
/// than before: an Athenaeum master built BEFORE the per-channel cards existed
/// carries `ATH_FNRM` but not `ATH_FNR/G/B`, so a CFA run recomputes from its
/// pixels for every light instead of reading one card. Rebuilding that master
/// stamps the cards and the reads go away; a master built since always has them.
pub fn resolve_flat_norm_divisor(
    flat_path: &Path,
    scratch_dir: &Path,
    mode: FlatNormMode,
    params: &LightCalParams,
    cfa: Option<CfaGeometry>,
) -> Result<FlatNormDivisor, IntegrationError> {
    let global =
        || flat_norm_constant(flat_path, scratch_dir, mode, params.trim_fraction).map(FlatNormDivisor::Global);

    if !params.cfa_flat_scaling {
        return global();
    }
    let Some(geom) = cfa else {
        // Mono: nothing to separate. Not worth a log line — it is the majority
        // of frames and says nothing about this run.
        return global();
    };
    if mode == FlatNormMode::PixinsightTrimmed {
        tracing::debug!(
            path = %flat_path.display(),
            "per-channel flat scaling ignored in pixinsightTrimmed mode (whole-frame statistic)"
        );
        return global();
    }

    let k = match read_ath_channel_norms(flat_path, geom) {
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

/// Best-effort read of a master flat's per-channel `ATH_FNR`/`ATH_FNG`/`ATH_FNB`
/// cards. All three must be present and usable as divisors (finite, > 0) — a
/// partial or broken triple counts as absent, so the caller recomputes from the
/// pixels rather than mixing a stamped constant with a recomputed one.
///
/// **The stamped constants are only usable if the flat's mosaic phase matches
/// the light's.** They were measured under the FLAT set's own consensus
/// geometry, while the pixel loop selects among them by the LIGHT's geometry —
/// so a disagreement (say the flat's members declared no `XBAYROFF`, making the
/// build assume 0, while this light declares 1) would feed every R pixel the B
/// constant and vice versa, silently, with `cfa_scaling_applied = true`
/// claiming it went fine. `build_master_cards` writes `BAYERPAT` (+ the offsets
/// it was given) into this same header in the same branch that produces the
/// constants, so the phase they were measured under is recoverable right here,
/// from the parse already being done. On a mismatch this returns `None`, which
/// drops the caller onto the recompute path — self-consistent by construction,
/// because that path measures the flat's pixels under the LIGHT's geometry.
///
/// A missing `XBAYROFF`/`YBAYROFF` reads as 0: that is exactly what
/// `measure_flat_channel_norms` assumed when it computed the constants, so it
/// reconstructs the build-time phase rather than guessing a new one. A flat
/// with the constants but no `BAYERPAT` cannot be verified at all and is
/// treated as a mismatch — unreachable for an Athenaeum-built master (the
/// pattern and the constants ride together, both gated on the same parse).
fn read_ath_channel_norms(flat_path: &Path, light: CfaGeometry) -> Option<[f64; 3]> {
    let (_, header_text) = parse_fits_with_header(flat_path, 0).ok()?;
    let keys = parse_stored_header_keys(FileFormat::FITS, &header_text);
    let read = |kw: &str| -> Option<f64> {
        keys.get(kw)
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|n| n.is_finite() && *n > 0.0)
    };
    let k = [read("ATH_FNR")?, read("ATH_FNG")?, read("ATH_FNB")?];

    let offset = |kw: &str| -> i64 {
        keys.get(kw)
            .and_then(|s| s.trim().parse::<i64>().ok())
            .unwrap_or(0)
    };
    let flat_geom = keys
        .get("BAYERPAT")
        .and_then(|p| Bayer::parse(p))
        .map(|pattern| CfaGeometry {
            pattern,
            xoff: offset("XBAYROFF"),
            yoff: offset("YBAYROFF"),
        });
    // Offsets compare modulo 2 ([`CfaGeometry::same_phase`], the single home of
    // the parity rule): an `XBAYROFF` of 2 and one of 0 are the same phase, and
    // calling that a disagreement would push a perfectly good flat onto the
    // recompute path for nothing.
    match flat_geom {
        Some(g) if g.same_phase(light) => Some(k),
        _ => {
            tracing::warn!(
                path = %flat_path.display(),
                flat_pattern = flat_geom.map(|g| g.pattern.as_str()).unwrap_or("none"),
                light_pattern = light.pattern.as_str(),
                "flat card geometry disagrees with light — per-channel constants recomputed from the flat plane"
            );
            None
        }
    }
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
    // One flat plane, one frame — already 1-2 bands at the floor, so the
    // machine-resolved budget would not change the band count here (spec §8).
    let band_rows = src.band_rows_for_budget(MIN_BUDGET_BYTES).min(h);
    let mut data = vec![0f32; w * h];
    let mut planes = BandPlanes::new(&src);
    let mut y = 0;
    while y < h {
        let rows = band_rows.min(h - y);
        src.read_band(y, rows, &mut planes)?;
        planes.decode_frame_into(0, &mut data[y * w..(y + rows) * w]);
        y += rows;
    }
    Ok((w, h, data))
}

/// Two-sided trimmed mean over the whole plane, discarding exactly
/// `trim_fraction` of the pixels from EACH tail (a common two-sided clipping
/// convention for flat normalization, spec §2; `trim_fraction` defaults to
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
    use crate::fits_writer::keywords::Bayer;
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
        let mut planes = BandPlanes::new(&src);
        src.read_band(0, h, &mut planes).unwrap();
        let mut data = vec![0f32; w * h];
        planes.decode_frame_into(0, &mut data);
        (w, h, data)
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
            // Mono by default: the CFA fixtures below opt in explicitly, so
            // every pre-existing expectation stays on the global path.
            cfa_geometry: None,
            params: LightCalParams::default(),
            // The fixtures below assert against `expect_px`, which mirrors the
            // engine with this same divisor; the per-BITPIX resolution itself is
            // covered by `scale_divisor_follows_source_bit_depth`.
            scale_divisor: OUTPUT_SCALE_DIVISOR,
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
    /// `flat_norm` off the divisor stays 1.0, so `flat_px` ARE the denominators;
    /// with it on the flat carries no `ATH_FNRM` card, so the divisor is the
    /// recomputed central-third mean — which on a 2x2 plane is `flat_px[0]`.
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
    fn floor_applies_with_flat_normalization_on() {
        // The default-ON mode: the floor is compared against the NORMALIZED
        // denominator (flat / ATH_FNRM), not the raw flat value. Divisor =
        // central-third mean = flat_px[0] = 1000.0, so the healthy pixels
        // normalize to 1.0 while the dead (0.0) and negative (-5.0) ones stay
        // below the floor and must be floored exactly as in the off mode.
        let (outcome, data) = run_engine_with_flat(&[1000.0, 0.0, -5.0, 1000.0], true);
        assert_eq!(outcome.calstat, "F");
        assert!(
            (outcome.flat_norm_divisor - 1000.0).abs() < 1e-9,
            "divisor {} must be the recomputed central-third mean",
            outcome.flat_norm_divisor
        );
        assert_eq!(outcome.floored_flat_pixels, 2, "exactly the dead + negative pixels");
        assert!(data.iter().all(|v| v.is_finite()), "no Inf/NaN survives the floor: {data:?}");
        assert!(data.iter().all(|&v| v > 0.0), "no sign flips: {data:?}");

        let floored = expect_px(100.0, None, Some(FLAT_DENOM_FLOOR), 1.0);
        assert_eq!(data[1], floored, "dead flat pixel must divide by the floor");
        assert_eq!(data[2], floored, "negative flat pixel must divide by the floor");
        // The healthy pixels normalize to exactly 1.0 — untouched by the floor.
        let healthy = expect_px(100.0, None, Some(1000.0), 1000.0);
        assert_eq!(data[0], healthy);
        assert_eq!(data[3], healthy);
    }

    #[test]
    fn denominator_exactly_at_the_floor_is_not_floored() {
        // Boundary pin on the `denom >= FLAT_DENOM_FLOOR` test. An f32 flat
        // pixel can never *be* 2.0e-5 exactly, but a normalized one can:
        // 2.0 / 100000.0 rounds to the same f64 as the FLAT_DENOM_FLOOR
        // literal, so pixel 1 sits exactly ON the boundary. It must take the
        // unfloored arm (count 0) while landing on the arithmetically
        // identical value the floored arm would have produced.
        let (outcome, data) = run_engine_with_flat(&[100000.0, 2.0, 100000.0, 100000.0], true);
        assert_eq!(2.0f64 / 100000.0, FLAT_DENOM_FLOOR, "fixture must sit exactly on the floor");
        assert_eq!(
            outcome.floored_flat_pixels, 0,
            "a denominator equal to the floor is not a floored pixel"
        );
        assert_eq!(
            data[1],
            expect_px(100.0, None, Some(FLAT_DENOM_FLOOR), 1.0),
            "on-boundary pixel must equal the floored-arm value"
        );
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
    fn scale_divisor_follows_source_bit_depth() {
        assert_eq!(scale_divisor_for_bitpix(Some(8)), 255.0);
        assert_eq!(scale_divisor_for_bitpix(Some(16)), 65535.0);
        assert_eq!(scale_divisor_for_bitpix(Some(32)), 4294967295.0);
        assert_eq!(scale_divisor_for_bitpix(Some(-32)), 1.0);
        assert_eq!(scale_divisor_for_bitpix(Some(-64)), 1.0);
        // Unknown depth (spill-path formats, unreadable header) keeps the
        // historic 16-bit divisor rather than guessing.
        assert_eq!(scale_divisor_for_bitpix(None), 65535.0);
        assert_eq!(scale_divisor_for_bitpix(Some(0)), OUTPUT_SCALE_DIVISOR);
    }

    // ── Per-channel (CFA) flat scaling ───────────────────────────────────────

    /// Paint a `w`x`h` RGGB mosaic at zero phase with one constant per colour.
    /// Painted from the pattern definition directly (even/even = R, odd/odd = B,
    /// the diagonal = G), NOT via `cfa_channel_at`, so the assertions test the
    /// engine's mapping instead of restating it.
    fn rggb_fill(r: f32, g: f32, blue: f32) -> impl Fn(usize, usize) -> f32 {
        move |x, y| match (x % 2 == 0, y % 2 == 0) {
            (true, true) => r,
            (false, false) => blue,
            _ => g,
        }
    }

    fn rggb_geom() -> CfaGeometry {
        CfaGeometry {
            pattern: Bayer::Rggb,
            xoff: 0,
            yoff: 0,
        }
    }

    /// Run the engine over an RGGB light divided by an RGGB flat, with
    /// per-channel scaling on or off. No dark; the flat carries no `ATH_FN*`
    /// cards, so both modes exercise the recompute-from-pixels path.
    fn run_cfa(
        w: usize,
        h: usize,
        light: (f32, f32, f32),
        flat: (f32, f32, f32),
        cfa_flat_scaling: bool,
    ) -> (tempfile::TempDir, LightCalOutcome, Vec<f32>) {
        let dir = tempfile::tempdir().unwrap();
        let light_path = write_fill(dir.path(), "light.fits", w, h, rggb_fill(light.0, light.1, light.2), &[]);
        let flat_path = write_fill(dir.path(), "flat.fits", w, h, rggb_fill(flat.0, flat.1, flat.2), &[]);
        let out = dir.path().join("out.fits");
        let mut cfg = inputs(dir.path(), light_path, None, None, Some(flat_path), true, out.clone());
        cfg.params.cfa_flat_scaling = cfa_flat_scaling;
        cfg.cfa_geometry = Some(rggb_geom());

        let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
        let (_, _, data) = read_all(&out, dir.path());
        (dir, outcome, data)
    }

    /// THE tool-parity pin. A flat that carries a strong colour of its own (the
    /// normal case: a CFA flat's R/G/B levels differ by the sensor's own colour
    /// response) must divide each colour by ITS OWN level, so the light's
    /// channel ratios survive calibration.
    ///
    /// Fixture: light R=1000 / G=2000 / B=500, flat R=2000 / G=4000 / B=1000 —
    /// the flat is exactly 2x the light in every channel. Per-channel: every
    /// pixel divides by its own channel constant (denominator exactly 1.0), so
    /// the output IS the light, ratios 2:4:1 intact. Globally: one constant
    /// (the central-third blend 2750) divides all three, and because this flat's
    /// colour matches the light's, every channel lands on the SAME value —
    /// 1375/65535 — the light's colour flattened out of existence. That
    /// collapse is the bug this feature fixes, so both arms are pinned exactly.
    #[test]
    fn per_channel_scaling_preserves_the_lights_channel_ratios() {
        let (w, h) = (6usize, 6usize);
        let (light, flat) = ((1000.0, 2000.0, 500.0), (2000.0, 4000.0, 1000.0));

        // ---- per-channel ON ----
        let (dir, outcome, data) = run_cfa(w, h, light, flat, true);
        assert_eq!(outcome.calstat, "F");
        assert!(outcome.cfa_scaling_applied, "an RGGB light + flat must scale per channel");
        // The per-channel constants are the painted flat levels, so the
        // global-equivalent number is their mosaic-weighted blend — and on this
        // fixture that lands exactly on the central-third global constant
        // (2750, asserted below), which is the point of weighting by G twice.
        let want_blend = (2000.0 + 2.0 * 4000.0 + 1000.0) / 4.0;
        assert_eq!(want_blend, 2750.0);
        assert!(
            (outcome.flat_norm_divisor - want_blend).abs() < 1e-9,
            "global-equivalent divisor {} must be the mosaic-weighted blend",
            outcome.flat_norm_divisor
        );

        // Bit-exact per pixel, through the same f64 mirror the global tests use:
        // each channel's denominator is flat_c / k_c = 1.0.
        let expect_on = |px: f64, k: f64| expect_px(px, None, Some(k), k);
        let on_r = expect_on(1000.0, 2000.0);
        let on_g = expect_on(2000.0, 4000.0);
        let on_b = expect_on(500.0, 1000.0);
        for y in 0..h {
            for x in 0..w {
                let want = match (x % 2 == 0, y % 2 == 0) {
                    (true, true) => on_r,
                    (false, false) => on_b,
                    _ => on_g,
                };
                assert_eq!(data[y * w + x], want, "per-channel pixel ({x},{y})");
            }
        }
        // The whole point: the light's colour ratios survive. 2:4:1 in, 2:4:1 out.
        assert!((on_g as f64 / on_r as f64 - 2.0).abs() < 1e-6, "G/R must stay 2");
        assert!((on_r as f64 / on_b as f64 - 2.0).abs() < 1e-6, "R/B must stay 2");
        drop(dir);

        // ---- per-channel OFF (global, the pre-feature behavior) ----
        let (dir, outcome, data) = run_cfa(w, h, light, flat, false);
        assert!(!outcome.cfa_scaling_applied, "the toggle must turn it off");
        // Central-third window is x,y in 2..4 → one R, two G, one B.
        let global = (2000.0 + 4000.0 + 4000.0 + 1000.0) / 4.0;
        assert_eq!(global, 2750.0, "fixture's global constant");
        assert!((outcome.flat_norm_divisor - global).abs() < 1e-9);

        let off_r = expect_px(1000.0, None, Some(2000.0), global);
        let off_g = expect_px(2000.0, None, Some(4000.0), global);
        let off_b = expect_px(500.0, None, Some(1000.0), global);
        for y in 0..h {
            for x in 0..w {
                let want = match (x % 2 == 0, y % 2 == 0) {
                    (true, true) => off_r,
                    (false, false) => off_b,
                    _ => off_g,
                };
                assert_eq!(data[y * w + x], want, "global pixel ({x},{y})");
            }
        }
        // …and this is what that costs: the flat's colour has white-balanced the
        // light's away — all three channels land on one value.
        assert_eq!(off_r, off_g, "global scaling flattens R and G together");
        assert_eq!(off_r, off_b, "global scaling flattens R and B together");
        assert_eq!(off_r, expect_px(1000.0, None, Some(2000.0), 2750.0));
        drop(dir);
    }

    /// A master flat as `build_master_cards` writes one: the per-channel
    /// constants ride WITH the `BAYERPAT` (+ offsets) they were measured under,
    /// because that phase is what makes them interpretable.
    fn master_flat_cards(pattern: &str, xoff: Option<i64>, yoff: Option<i64>, k: [f64; 3]) -> Vec<Card> {
        let mut cards = vec![
            Card::new("BAYERPAT", CardValue::Str(pattern.into())).unwrap(),
            Card::new("ATH_FNRM", CardValue::Real(2750.0)).unwrap(),
            Card::new("ATH_FNR", CardValue::Real(k[0])).unwrap(),
            Card::new("ATH_FNG", CardValue::Real(k[1])).unwrap(),
            Card::new("ATH_FNB", CardValue::Real(k[2])).unwrap(),
        ];
        // Offsets are stamped only when the members declared them — an absent
        // card means the build assumed 0, exactly as this reader does.
        if let Some(x) = xoff {
            cards.push(Card::new("XBAYROFF", CardValue::Integer(x)).unwrap());
        }
        if let Some(y) = yoff {
            cards.push(Card::new("YBAYROFF", CardValue::Integer(y)).unwrap());
        }
        cards
    }

    /// The stamped `ATH_FNR`/`ATH_FNG`/`ATH_FNB` cards win over recomputation,
    /// exactly as `ATH_FNRM` does for the global constant. Cards deliberately
    /// disagree with the pixels (10x off) so precedence is visible.
    #[test]
    fn per_channel_constants_read_from_the_master_cards_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (6usize, 6usize);
        let light = write_fill(dir.path(), "light.fits", w, h, rggb_fill(1000.0, 2000.0, 500.0), &[]);
        let cards = master_flat_cards("RGGB", Some(0), Some(0), [200.0, 400.0, 100.0]);
        let flat = write_fill(dir.path(), "flat.fits", w, h, rggb_fill(2000.0, 4000.0, 1000.0), &cards);
        let out = dir.path().join("out.fits");
        let mut cfg = inputs(dir.path(), light, None, None, Some(flat), true, out.clone());
        cfg.cfa_geometry = Some(rggb_geom());

        let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
        assert!(outcome.cfa_scaling_applied);
        assert_eq!(
            outcome.flat_channel_divisors,
            Some([200.0, 400.0, 100.0]),
            "the stamped cards must win over recomputation"
        );
        let (_, _, data) = read_all(&out, dir.path());
        // R pixel: 1000 / (2000/200) = 100, i.e. 10x the recomputed answer.
        assert_eq!(data[0], expect_px(1000.0, None, Some(2000.0), 200.0));
    }

    /// The stamped constants are only meaningful under the phase they were
    /// MEASURED in. The flat's cards say RGGB at phase (0,0); this light
    /// declares xoff=1 — so index 0 of the triple is the flat's R while pixel
    /// (0,0) of the light is a G. Using the cards anyway would divide every
    /// channel by another channel's level and report success. The engine must
    /// fall through to recomputation, which measures the flat's own pixels under
    /// the LIGHT's geometry and is therefore self-consistent by construction.
    #[test]
    fn card_constants_rejected_when_flat_phase_disagrees_with_the_light() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (6usize, 6usize);
        let light = write_fill(dir.path(), "light.fits", w, h, rggb_fill(1000.0, 2000.0, 500.0), &[]);
        let cards = master_flat_cards("RGGB", Some(0), Some(0), [200.0, 400.0, 100.0]);
        let flat = write_fill(dir.path(), "flat.fits", w, h, rggb_fill(2000.0, 4000.0, 1000.0), &cards);
        let out = dir.path().join("out.fits");
        let mut cfg = inputs(dir.path(), light, None, None, Some(flat), true, out.clone());
        cfg.cfa_geometry = Some(CfaGeometry {
            pattern: Bayer::Rggb,
            xoff: 1, // one column out of phase with the flat's cards
            yoff: 0,
        });

        let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
        assert!(outcome.cfa_scaling_applied, "still per-channel, just not from the cards");
        // Recomputed under xoff=1 over the painted 6x6 mosaic: the window's
        // pixels relabel to G, R, B, G, giving [4000, 1500, 4000] — nothing like
        // the stamped [200, 400, 100], so the assertion cannot pass by accident.
        let k = outcome.flat_channel_divisors.expect("per-channel constants");
        assert_eq!(k, [4000.0, 1500.0, 4000.0], "must be recomputed under the light's phase");
        assert_ne!(k, [200.0, 400.0, 100.0], "the disagreeing cards must not be used");

        // A pattern mismatch is rejected for the same reason, phase aside.
        let mut cfg2 = LightCalInputs {
            output_path: dir.path().join("out2.fits"),
            ..cfg
        };
        cfg2.cfa_geometry = Some(CfaGeometry {
            pattern: Bayer::Bggr,
            xoff: 0,
            yoff: 0,
        });
        let outcome2 = calibrate_light(&cfg2, &AtomicBool::new(false)).unwrap();
        assert_ne!(
            outcome2.flat_channel_divisors,
            Some([200.0, 400.0, 100.0]),
            "a different pattern must not read the flat's cards either"
        );
    }

    /// An offset that differs only by a multiple of 2 is the SAME phase —
    /// `cfa_channel_at` folds offsets modulo 2 — so it must not be treated as a
    /// disagreement and must keep using the stamped cards.
    #[test]
    fn offsets_congruent_modulo_two_still_use_the_cards() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (6usize, 6usize);
        let light = write_fill(dir.path(), "light.fits", w, h, rggb_fill(1000.0, 2000.0, 500.0), &[]);
        let cards = master_flat_cards("RGGB", Some(0), Some(0), [200.0, 400.0, 100.0]);
        let flat = write_fill(dir.path(), "flat.fits", w, h, rggb_fill(2000.0, 4000.0, 1000.0), &cards);
        let out = dir.path().join("out.fits");
        let mut cfg = inputs(dir.path(), light, None, None, Some(flat), true, out.clone());
        cfg.cfa_geometry = Some(CfaGeometry {
            pattern: Bayer::Rggb,
            xoff: 2,
            yoff: -2,
        });

        let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
        assert_eq!(
            outcome.flat_channel_divisors,
            Some([200.0, 400.0, 100.0]),
            "phase (2,-2) is phase (0,0) — the cards stay usable"
        );
    }

    /// A flat carrying the constants but NO `BAYERPAT` cannot have its phase
    /// verified, so the constants are unusable and the engine recomputes.
    /// (`build_master_cards` never emits that shape — both come out of the same
    /// parse — so this guards a hand-edited or foreign header.)
    #[test]
    fn card_constants_without_a_pattern_are_not_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (6usize, 6usize);
        let light = write_fill(dir.path(), "light.fits", w, h, rggb_fill(1000.0, 2000.0, 500.0), &[]);
        let cards = [
            Card::new("ATH_FNRM", CardValue::Real(2750.0)).unwrap(),
            Card::new("ATH_FNR", CardValue::Real(200.0)).unwrap(),
            Card::new("ATH_FNG", CardValue::Real(400.0)).unwrap(),
            Card::new("ATH_FNB", CardValue::Real(100.0)).unwrap(),
        ];
        let flat = write_fill(dir.path(), "flat.fits", w, h, rggb_fill(2000.0, 4000.0, 1000.0), &cards);
        let out = dir.path().join("out.fits");
        let mut cfg = inputs(dir.path(), light, None, None, Some(flat), true, out.clone());
        cfg.cfa_geometry = Some(rggb_geom());

        let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
        assert_eq!(
            outcome.flat_channel_divisors,
            Some([2000.0, 4000.0, 1000.0]),
            "unverifiable constants must be recomputed, not trusted"
        );
    }

    /// A mono light (no CFA geometry) is untouched by the toggle: same output,
    /// same row flag, whatever `cfa_flat_scaling` says.
    #[test]
    fn mono_light_is_unaffected_by_the_toggle() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (6usize, 6usize);
        let light = write_plane(dir.path(), "light.fits", w, h, 1000.0, &[]);
        let flat = write_plane(dir.path(), "flat.fits", w, h, 2000.0, &[]);

        let mut outputs = Vec::new();
        for (i, on) in [true, false].into_iter().enumerate() {
            let out = dir.path().join(format!("out{i}.fits"));
            let mut cfg = inputs(
                dir.path(),
                light.clone(),
                None,
                None,
                Some(flat.clone()),
                true,
                out.clone(),
            );
            cfg.params.cfa_flat_scaling = on;
            cfg.cfa_geometry = None; // mono: no pattern declared
            let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
            assert!(!outcome.cfa_scaling_applied, "mono can never scale per channel");
            assert!(outcome.flat_channel_divisors.is_none());
            let (_, _, data) = read_all(&out, dir.path());
            outputs.push(data);
        }
        assert_eq!(outputs[0], outputs[1], "mono output must not depend on the toggle");
        assert_eq!(outputs[0][0], expect_px(1000.0, None, Some(2000.0), 2000.0));
    }

    /// `pixinsightTrimmed` is a whole-frame parity statistic — per-channel
    /// scaling is ignored there, so the output matches the toggle-off run
    /// bit-for-bit and the row records that nothing per-channel was applied.
    #[test]
    fn pixinsight_trimmed_mode_ignores_per_channel_scaling() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (6usize, 6usize);
        let light = write_fill(dir.path(), "light.fits", w, h, rggb_fill(1000.0, 2000.0, 500.0), &[]);
        let flat = write_fill(dir.path(), "flat.fits", w, h, rggb_fill(2000.0, 4000.0, 1000.0), &[]);

        let mut outputs = Vec::new();
        for (i, on) in [true, false].into_iter().enumerate() {
            let out = dir.path().join(format!("out{i}.fits"));
            let mut cfg = inputs(
                dir.path(),
                light.clone(),
                None,
                None,
                Some(flat.clone()),
                true,
                out.clone(),
            );
            cfg.flat_norm_mode = FlatNormMode::PixinsightTrimmed;
            cfg.params.cfa_flat_scaling = on;
            cfg.cfa_geometry = Some(rggb_geom());
            let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
            assert!(
                !outcome.cfa_scaling_applied,
                "pixinsightTrimmed must stay whole-frame (toggle = {on})"
            );
            let (_, _, data) = read_all(&out, dir.path());
            outputs.push(data);
        }
        assert_eq!(outputs[0], outputs[1], "the toggle must not change PI-mode output");
    }

    /// A degenerate channel constant (the flat has no pixels of one colour at
    /// any usable level) must not divide a whole channel by garbage: the frame
    /// falls back to the GLOBAL constant as a whole — never mixed-mode, where
    /// two channels would be per-channel-scaled and the third not.
    #[test]
    fn degenerate_channel_falls_back_to_global_for_the_whole_frame() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (6usize, 6usize);
        let light = write_fill(dir.path(), "light.fits", w, h, rggb_fill(1000.0, 2000.0, 500.0), &[]);
        // B is 0.0 everywhere → its channel mean is 0.0, unusable as a divisor.
        let flat = write_fill(dir.path(), "flat.fits", w, h, rggb_fill(2000.0, 4000.0, 0.0), &[]);
        let out = dir.path().join("out.fits");
        let mut cfg = inputs(dir.path(), light, None, None, Some(flat), true, out.clone());
        cfg.cfa_geometry = Some(rggb_geom());

        let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
        assert!(!outcome.cfa_scaling_applied, "a degenerate channel disables per-channel scaling");
        assert!(outcome.flat_channel_divisors.is_none());
        // Global constant over the central third: one R (2000), two G (4000),
        // one B (0) → 2500.
        assert!((outcome.flat_norm_divisor - 2500.0).abs() < 1e-9, "{}", outcome.flat_norm_divisor);
        let (_, _, data) = read_all(&out, dir.path());
        // The healthy R channel is scaled globally too — not per-channel.
        assert_eq!(data[0], expect_px(1000.0, None, Some(2000.0), 2500.0));
    }

    /// The band loop indexes pixels within a band, but the CFA phase is a
    /// property of the frame's GLOBAL row. A band whose height is odd shifts
    /// every following band's phase by one row, so a band-local `y` swaps R and
    /// B from band 2 onward. Forced multi-band here via a tall frame and a tiny
    /// budget-equivalent, and asserted against the same painted expectation the
    /// single-band test uses.
    #[test]
    fn multi_band_run_keeps_the_global_cfa_row_phase() {
        // A frame tall enough that the engine's real budget still yields one
        // band would prove nothing, so drive the band size directly.
        let dir = tempfile::tempdir().unwrap();
        // 6 wide, not 4: the central-third window of a 4-wide frame is a single
        // column, which contains no R pixel at all — the constants would come
        // back degenerate and the run would (correctly) fall back to global,
        // proving nothing about band phase.
        let (w, h) = (6usize, 12usize);
        let light = write_fill(dir.path(), "light.fits", w, h, rggb_fill(1000.0, 2000.0, 500.0), &[]);
        let flat = write_fill(dir.path(), "flat.fits", w, h, rggb_fill(2000.0, 4000.0, 1000.0), &[]);
        let out = dir.path().join("out.fits");

        // 3 rows per band → 4 bands, and an ODD band height so a band-local row
        // index lands on the wrong phase from band 2 onward. Driven through the
        // budget the same way `calibrate_light_compute_inner` is: per_row =
        // width*4 (light, f32) + width*4 (flat, f32) + width*8 headroom = 96
        // bytes here, so 300 bytes buys exactly 3 rows — this fixture happens to
        // land on the same number the old (n+2)*w*4 formula gave, since both
        // frames here are f32.
        let probe = BandSource::open(&[light.clone(), flat.clone()], dir.path()).unwrap();
        assert_eq!(probe.band_rows_for_budget(300), 3, "fixture must produce 3-row bands");

        let mut cfg = inputs(dir.path(), light, None, None, Some(flat), true, out.clone());
        cfg.cfa_geometry = Some(rggb_geom());

        let outcome = calibrate_light_inner(&cfg, &AtomicBool::new(false), 300).unwrap();
        assert!(outcome.cfa_scaling_applied);
        let (_, _, data) = read_all(&out, dir.path());
        let on_r = expect_px(1000.0, None, Some(2000.0), 2000.0);
        let on_g = expect_px(2000.0, None, Some(4000.0), 4000.0);
        let on_b = expect_px(500.0, None, Some(1000.0), 1000.0);
        for y in 0..h {
            for x in 0..w {
                let want = match (x % 2 == 0, y % 2 == 0) {
                    (true, true) => on_r,
                    (false, false) => on_b,
                    _ => on_g,
                };
                assert_eq!(data[y * w + x], want, "pixel ({x},{y}) in band {}", y / 3);
            }
        }
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

    /// The compute/write split is a pure refactor: running the two phases by
    /// hand must produce the SAME file bytes and the SAME outcome as the
    /// one-shot [`calibrate_light`]. Pins the seam a later task inserts
    /// hot-pixel correction and debayering into — if the split ever starts
    /// dropping a step (the flat divisor, the pedestal, the floored counter,
    /// the header cards), this diverges.
    #[test]
    fn compute_then_write_equals_calibrate_light() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (8usize, 9usize);
        // Full BDF path with a non-trivial flat, a real header card and a
        // pedestal, so every field the split carries across is exercised.
        let light = write_fill(dir.path(), "light.fits", w, h, |x, y| 1000.0 + (x + y * w) as f32, &[]);
        let dark = write_plane(dir.path(), "dark.fits", w, h, 100.0, &[]);
        // One dead flat pixel so the floored counter is NON-zero: comparing
        // `0 == 0` would pass even if the split dropped the counter entirely.
        // The `ATH_FNRM` card still fixes the divisor at 2.0 (card path, no
        // recompute), so the dead pixel does not move `flat_norm_divisor`.
        let flat = write_fill(
            dir.path(),
            "flat.fits",
            w,
            h,
            |x, y| if (x, y) == (3, 4) { 0.0 } else { 2.0 },
            &[fnrm_card(2.0)],
        );
        let cards = vec![Card::new("OBJECT", CardValue::Str("M42".into())).unwrap()];

        let out_a = dir.path().join("out_a.fits");
        let mut cfg_a = inputs(
            dir.path(),
            light.clone(),
            Some(dark.clone()),
            None,
            Some(flat.clone()),
            true,
            out_a.clone(),
        );
        cfg_a.params.pedestal_dn = 50.0;
        cfg_a.cards = cards.clone();
        let outcome_a = calibrate_light(&cfg_a, &AtomicBool::new(false)).unwrap();

        let out_b = dir.path().join("out_b.fits");
        let mut cfg_b = inputs(dir.path(), light, Some(dark), None, Some(flat), true, out_b.clone());
        cfg_b.params.pedestal_dn = 50.0;
        cfg_b.cards = cards;
        let (frame, outcome_b) = calibrate_light_compute(&cfg_b, &AtomicBool::new(false)).unwrap();
        // The compute phase writes nothing and hashes nothing.
        assert!(outcome_b.output_hash.is_empty(), "compute must not hash");
        assert!(!out_b.exists(), "compute must not write");
        assert_eq!((frame.width, frame.height), (w, h));
        assert_eq!(frame.data.len(), w * h);
        let hash_b = write_calibrated_output(
            &out_b,
            frame.width,
            frame.height,
            1,
            &frame.data,
            &cfg_b.cards,
        )
        .unwrap();

        assert_eq!(std::fs::read(&out_a).unwrap(), std::fs::read(&out_b).unwrap(), "file bytes differ");
        assert_eq!(outcome_a.output_hash, hash_b, "hash differs");
        assert_eq!(outcome_a.calstat, outcome_b.calstat);
        assert_eq!(outcome_a.flat_norm_divisor, outcome_b.flat_norm_divisor);
        assert_eq!(outcome_a.flat_channel_divisors, outcome_b.flat_channel_divisors);
        assert_eq!(outcome_a.cfa_scaling_applied, outcome_b.cfa_scaling_applied);
        assert_eq!(outcome_a.floored_flat_pixels, outcome_b.floored_flat_pixels);
        assert!(outcome_a.floored_flat_pixels > 0, "fixture must exercise the floor");
    }
}

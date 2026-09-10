//! Local normalization (M2, spec §5.2): per-frame `A(x,y)`/`B(x,y)` grids in
//! the reference geometry, applied as `v' = A·v + B`. `grid` holds the grid
//! type itself (`LnGrid`), the bicubic B-spline evaluator that turns the
//! coarse stride grid into per-pixel `A`/`B` values, and the `.athln` binary
//! sidecar one frame's grids (one per channel, `LnFrameGrids`) round-trip
//! through. `background` (M2 Task 2) is the per-plane robust background
//! model on the same stride mesh — the input Task 5 builds `B = B_ref −
//! s·B_tgt` from. `scale` (M2 Task 3) is the frame-global multiplicative
//! term `s` — the RCR location of matched-star PSF-flux ratios — Task 5
//! stamps onto the grid as `A`. `reference` (M2 Task 4) is the per-group
//! low-noise reference (`LnReference`) Task 5 measures background/scale
//! against — built by sharing `stacking::integrate::integrate_planes`, the
//! same per-plane engine loop `integrate_group` (Plan 4) drives.
//!
//! [`normalize_frame`] (M2 Task 5) is the per-frame driver: it warps one
//! calibrated frame into the reference geometry (a one-frame
//! [`crate::integration::registered_source::RegisteredSource`], whole plane
//! per channel), models the target's background the same way the reference's
//! own was modelled (just a looser deviation threshold —
//! [`background::TARGET_DEVIATION_SIGMA`]), takes the PSF relative scale
//! against the SAME channel of the reference, and folds the two into one
//! [`LnGrid`] per channel (`A = s`, `B = B_ref − s·B_tgt`) written as one
//! `.athln` sidecar. `stacking::run` (the orchestration layer, spec §9.3)
//! owns resolving/caching the [`LnReference`] itself, fanning this out over a
//! group's included frames, and recording the outcome in the run's
//! provenance — none of that DB/artifact bookkeeping belongs in this
//! low-level module.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::integration::banded::BandPlanes;
use crate::integration::registered_source::{RegisteredFrame, RegisteredSource};
use crate::integration::source::FrameSource;
use crate::integration::stats::median_of;
use crate::integration::IntegrationError;
use crate::resample::Interpolation;
use crate::stacking::integrate::{LocalNormalizationConfig, StackFrame};
use crate::stacking::measure::MeasureOptions;

pub mod background;
pub mod grid;
pub mod reference;
pub mod scale;

pub use background::{
    background_grid, BackgroundGrid, BackgroundParams, DEFAULT_PARAMS, TARGET_DEVIATION_SIGMA,
};
pub use grid::{LnFrameGrids, LnGrid};
pub use reference::{build_reference, read_reference, write_reference, LnReference};
pub use scale::{relative_scale, ScaleResult};

/// Local-normalization errors shared by every M2 task past detection: a
/// frame that cannot be trusted for LN (too few matched stars — see
/// [`scale::relative_scale`]), sidecar/artifact I/O, or anything else a
/// later task's message doesn't warrant its own variant for.
#[derive(Debug)]
pub enum LnError {
    /// Fewer than [`scale::MIN_MATCHES`] star pairs survived matching. The
    /// run excludes the frame from local normalization with this exact
    /// message (`excluded: "local normalization: N matched stars (< 20)"`)
    /// unless its rejection algorithm is not `local`, in which case the frame
    /// keeps global normalization instead and this is logged as a warning.
    TooFewMatches {
        matches: usize,
    },
    Io(std::io::Error),
    Other(String),
}

impl std::fmt::Display for LnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LnError::TooFewMatches { matches } => {
                write!(
                    f,
                    "local normalization: {matches} matched stars (< {})",
                    scale::MIN_MATCHES
                )
            }
            LnError::Io(e) => write!(f, "local normalization: I/O error: {e}"),
            LnError::Other(msg) => write!(f, "local normalization: {msg}"),
        }
    }
}

impl std::error::Error for LnError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LnError::Io(e) => Some(e),
            LnError::TooFewMatches { .. } | LnError::Other(_) => None,
        }
    }
}

/// One frame's stage-6 outcome (M2 Task 5): where its sidecar landed, the
/// frame-level scale/matches/rejected-cells the run logs and reports in its
/// provenance (spec §9.3/§9.5) — averaged/summed across channels when the
/// frame carries more than one (a debayered OSC light): `scale` is the mean
/// of every channel's own [`scale::ScaleResult::scale`] (a single number for
/// `SummaryFrame.ln_scale`/the `FramesTable` UI column; each channel's own
/// value is still exactly what its own [`LnGrid::global_scale`] carries),
/// `matches`/`cells_rejected` are the SUM over channels (how much data
/// backed the whole frame's normalization, not just one channel's).
#[derive(Debug, Clone)]
pub struct LnFrameOutcome {
    pub sidecar: std::path::PathBuf,
    pub scale: f64,
    pub matches: usize,
    pub cells_rejected: usize,
}

/// Per-channel target background parameters (spec §5.2/math §4.2): same
/// scale/clip/hot-pixel settings as the reference's own
/// [`background::DEFAULT_PARAMS`], just the looser
/// [`background::TARGET_DEVIATION_SIGMA`] deviation threshold — a target
/// frame carries its own noise/registration residual on top of the
/// reference's.
fn target_background_params(scale: u32) -> BackgroundParams {
    BackgroundParams {
        scale,
        deviation_sigma: TARGET_DEVIATION_SIGMA,
        ..DEFAULT_PARAMS
    }
}

/// The median of ONLY the finite pixels of `plane` (fix round 1, item 7):
/// [`median_of`]'s `total_cmp`-based sort is a genuine total order even over
/// NaN, so it never panics — but a NaN-laden plane still SKEWS a whole-plane
/// median computed that way, since `total_cmp` sorts every NaN to one
/// extreme rather than treating it as absent (exactly what a warped frame's
/// off-canvas strip is: absent data, not a real low/high value). `f64::NAN`
/// when there is no finite pixel at all — the fully-invalid background-grid
/// refusal upstream (fix round 1, item 3) means this is never the only
/// signal of that case reaching a caller.
fn median_of_finite(plane: &[f32]) -> f64 {
    let finite: Vec<f32> = plane.iter().copied().filter(|v| v.is_finite()).collect();
    if finite.is_empty() {
        return f64::NAN;
    }
    median_of(&finite) as f64
}

/// The reference-side inputs [`normalize_frame`] needs for star detection,
/// computed ONCE PER GROUP (fix round 1, item 6) — not once per frame, as
/// the very first version of this module did (`sanitized_for_detection`
/// rebuilt the reference's own sanitized copy on every one of a group's
/// `normalize_frame` calls, for no reason: the reference itself never
/// changes across a group's frames). `stacking::run::run_group_normalization`
/// builds one of these right after resolving the group's [`LnReference`],
/// via [`Self::build`], and passes the SAME instance into every
/// `normalize_frame` call the group's fan-out makes.
///
/// `sanitized_planes[p]` is channel `p`'s reference plane with any
/// non-finite pixel replaced by `locations[p]` (fix round 1, item 6: the
/// plane's own finite MEDIAN, not a flat `0.0` — a zero floor would drag the
/// detector's global background/σ estimate once the non-finite region is
/// more than a few percent of the plane; a target frame's own off-canvas
/// strip gets the identical treatment, per-frame, for the same reason).
/// `locations[p]` is that channel's median over finite pixels only (fix
/// round 1, item 7 — see [`median_of_finite`]) — reused directly as
/// [`LnGrid::location_ref`] so it, too, is computed once, not once per
/// frame.
pub struct LnReferenceForDetection {
    pub sanitized_planes: Vec<Vec<f32>>,
    pub locations: Vec<f64>,
}

impl LnReferenceForDetection {
    pub fn build(reference: &LnReference) -> LnReferenceForDetection {
        let mut sanitized_planes = Vec::with_capacity(reference.planes.len());
        let mut locations = Vec::with_capacity(reference.planes.len());
        for plane in &reference.planes {
            let location = median_of_finite(plane);
            let sanitized = if plane.iter().all(|v| v.is_finite()) {
                plane.clone()
            } else {
                plane
                    .iter()
                    .map(|&v| if v.is_finite() { v } else { location as f32 })
                    .collect()
            };
            sanitized_planes.push(sanitized);
            locations.push(location);
        }
        LnReferenceForDetection {
            sanitized_planes,
            locations,
        }
    }
}

/// Warps `frame` into the reference geometry (one-frame [`RegisteredSource`],
/// whole plane per channel — a single band the height of the reference,
/// read once through the [`FrameSource`] trait exactly like every other
/// registered-frame consumer), models both backgrounds, takes the PSF scale
/// against the SAME channel of `reference`, builds `A = s`,
/// `B = B_ref − s·B_tgt` on the stride grid (spec §5.2/math §4.4) and writes
/// `<stem>.athln` at `sidecar` ([`LnFrameGrids::write`] — tmp file + atomic
/// rename, so a reader never observes a half-written sidecar).
///
/// `interpolation`/`clamping` are the SAME choices the group's own
/// registration uses (`GroupInput::interpolation`/`::clamping`, threaded in
/// by the caller) — not a field of `cfg`, since warping the target with a
/// DIFFERENT kernel than the one that built `reference` (`build_reference`
/// reads them from its own `GroupInput`) would bias the PSF-flux ratio by
/// however the two kernels' effective resolution differs, on top of the real
/// seeing difference the ratio is supposed to measure.
///
/// Fails fast with [`LnError::TooFewMatches`] the moment ANY channel's
/// [`scale::relative_scale`] call comes back short — the caller decides
/// whether that excludes the frame (LN drives OUTPUT normalization) or is
/// merely a warning (LN drives rejection only); a partially-written sidecar
/// (fewer channels than `reference` has planes) would leave stage 7 unable
/// to evaluate every plane's grid, so nothing is written on this path at
/// all. `cancel` is checked once per channel — the per-channel work
/// (star detection + PSF fitting on two planes) is the only part worth
/// interrupting early; a cancel noticed here surfaces as [`LnError::Other`],
/// which the caller (already checking its own cancel flag right after the
/// fan-out that calls this) does not need to interpret specially.
#[allow(clippy::too_many_arguments)]
pub fn normalize_frame(
    reference: &LnReference,
    reference_for_detection: &LnReferenceForDetection,
    ref_backgrounds: &[BackgroundGrid],
    frame: &StackFrame,
    cfg: &LocalNormalizationConfig,
    measure: &MeasureOptions,
    interpolation: Interpolation,
    clamping: f32,
    sidecar: &Path,
    cancel: &AtomicBool,
) -> Result<LnFrameOutcome, LnError> {
    let channels = reference.planes.len();
    if frame.measurement.channels.len() != channels {
        return Err(LnError::Other(format!(
            "frame has {} measured channels, the LN reference has {channels}",
            frame.measurement.channels.len()
        )));
    }
    if ref_backgrounds.len() != channels {
        return Err(LnError::Other(format!(
            "{} reference background grids for a {channels}-channel LN reference",
            ref_backgrounds.len()
        )));
    }
    if reference_for_detection.sanitized_planes.len() != channels
        || reference_for_detection.locations.len() != channels
    {
        return Err(LnError::Other(format!(
            "reference-for-detection has {}/{} channels, the LN reference has {channels}",
            reference_for_detection.sanitized_planes.len(),
            reference_for_detection.locations.len()
        )));
    }

    let stride = (cfg.scale / 8).max(2) as usize;
    let target_params = target_background_params(cfg.scale);

    let mut grids = Vec::with_capacity(channels);
    let mut scales = Vec::with_capacity(channels);
    let mut matches_total = 0usize;
    let mut cells_rejected_total = 0usize;

    for p in 0..channels {
        if cancel.load(Ordering::Relaxed) {
            return Err(LnError::Other("cancelled".to_string()));
        }

        let registered = RegisteredFrame {
            path: frame.path.clone(),
            map: frame.map.clone(),
        };
        let src = RegisteredSource::open(
            &[registered],
            reference.width,
            reference.height,
            p,
            interpolation,
            clamping,
        )
        .map_err(|e| LnError::Other(format!("warping into the reference geometry: {e}")))?;

        let mut band = BandPlanes::new(&src);
        let no_progress = |_: u64| {};
        src.read_band_with_progress(0, reference.height, &mut band, 1, &no_progress, cancel)
            .map_err(|e| match e {
                IntegrationError::Cancelled => LnError::Other("cancelled".to_string()),
                other => LnError::Other(format!("warping into the reference geometry: {other}")),
            })?;
        let mut target = vec![0f32; reference.width * reference.height];
        band.decode_frame_into(0, &mut target);

        let target_bg = background_grid(&target, reference.width, reference.height, &target_params);
        let (expected_gw, expected_gh) =
            LnGrid::grid_dims(reference.width, reference.height, stride);
        debug_assert_eq!(
            (target_bg.gw, target_bg.gh),
            (expected_gw, expected_gh),
            "target background grid must share the reference's own mesh"
        );

        // Fix round 1, item 3: `BackgroundGrid`'s own documented contract —
        // `invalid_cells == gw * gh` means the WHOLE plane had no measurable
        // cell, and the (all-zero) `cells` it still returns is a fallback
        // the caller "should refuse", never trust. Silently continuing here
        // used to yield `B = B_ref` everywhere and a written sidecar that
        // looks like a normal, if noisy, result.
        let target_cell_count = target_bg.gw * target_bg.gh;
        if target_cell_count > 0 && target_bg.invalid_cells == target_cell_count {
            return Err(LnError::Other(
                "background model: no measurable cell".to_string(),
            ));
        }
        cells_rejected_total += target_bg.invalid_cells;

        // Fix round 1, item 7: the median over FINITE pixels only — see
        // `median_of_finite`'s own doc for why a NaN-laden plane still needs
        // this even though `total_cmp`-based sorting never panics on NaN.
        let location_tgt = median_of_finite(&target);

        // Fix round 1, item 6: sanitize `target` IN PLACE for detection —
        // everything that needed the NaN-preserving version
        // (`background_grid`, `location_tgt`) has already read it, so
        // there is no separate copy to allocate. A frame's own footprint
        // rarely covers the WHOLE reference canvas once warped (a non-zero
        // registration shift leaves a strip outside the source frame's own
        // extent) — `RegisteredSource::fill_frame` fills exactly that strip
        // with NaN (see its own doc: "band maps outside the source"). The
        // detector underneath `relative_scale` is not NaN-safe — a NaN
        // reaching its own median/HFD math violates the total order Rust's
        // sort requires and panics — so detection needs a place-holder
        // value in that strip regardless; the plane's own finite median
        // (not a flat `0.0`) keeps a large strip from dragging the
        // detector's global background/σ estimate.
        if !target.iter().all(|v| v.is_finite()) {
            for v in target.iter_mut() {
                if !v.is_finite() {
                    *v = location_tgt as f32;
                }
            }
        }

        let scale_result = relative_scale(
            &reference_for_detection.sanitized_planes[p],
            &target,
            reference.width,
            reference.height,
            cfg.psf_model,
            measure.max_stars,
            4.0,
            0.3,
        )?;
        matches_total += scale_result.matches;
        scales.push(scale_result.scale);

        let ref_bg = &ref_backgrounds[p];
        // Fix round 1, item 8: a real length check, not just the
        // `debug_assert_eq!` above (which only pins `target_bg`'s OWN dims
        // against what the stride geometry implies — it says nothing about
        // `ref_bg` matching `target_bg`, e.g. a caller-supplied `ref_backgrounds`
        // built at a different scale). `zip` would otherwise silently
        // truncate to the shorter of the two in release builds.
        if ref_bg.cells.len() != target_bg.cells.len() {
            return Err(LnError::Other(format!(
                "background grid size mismatch: reference has {} cells, target has {}",
                ref_bg.cells.len(),
                target_bg.cells.len()
            )));
        }
        let s = scale_result.scale as f32;
        let b: Vec<f32> = ref_bg
            .cells
            .iter()
            .zip(target_bg.cells.iter())
            .map(|(&br, &bt)| br - s * bt)
            .collect();

        grids.push(LnGrid {
            ref_width: reference.width,
            ref_height: reference.height,
            scale: cfg.scale,
            gw: expected_gw,
            gh: expected_gh,
            a: vec![s; expected_gw * expected_gh],
            b,
            global_scale: scale_result.scale,
            location_ref: reference_for_detection.locations[p],
            location_tgt,
        });
    }

    LnFrameGrids { channels: grids }
        .write(sidecar)
        .map_err(|e| LnError::Other(format!("writing .athln sidecar: {e:#}")))?;

    let scale = scales.iter().sum::<f64>() / scales.len().max(1) as f64;

    Ok(LnFrameOutcome {
        sidecar: sidecar.to_path_buf(),
        scale,
        matches: matches_total,
        cells_rejected: cells_rejected_total,
    })
}

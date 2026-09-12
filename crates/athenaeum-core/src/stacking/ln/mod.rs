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
//! [`LnGrid`] per channel (`A = s` — or, with `localScale` on, the local
//! scale spline of ruling R-M4c-8 sampled node by node; `B = B_ref −
//! A·B_tgt`) written as one `.athln` sidecar. `stacking::run` (the
//! orchestration layer, spec §9.3) owns resolving/caching the
//! [`LnReference`] itself, fanning this out over a
//! group's included frames, and recording the outcome in the run's
//! provenance — none of that DB/artifact bookkeeping belongs in this
//! low-level module.

use std::borrow::Cow;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::geometry::ThinPlateSpline;
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
pub use scale::{
    relative_scale, relative_scale_against, PreparedReferenceChannel, ScaleResult,
    LN_BARYCENTRE_PASS_THRESHOLD, LN_LOCAL_SCALE_MIN_STARS, LN_LOCAL_SCALE_SMOOTHING_SIGMAS,
};

/// Safety band on the SAMPLED local-scale surface, as a fraction of the
/// global scale `s` (an addition to ruling R-M4c-8's own wording): a
/// flat-field residual that moves the relative scale by more than a
/// quarter of `s` between two corners of the same frame is not a
/// flat-field residual — it is a spline that left its node cloud or a fit
/// that went wrong. Such a surface is refused as a whole and the channel
/// keeps the constant `A = s`, loudly. Real vignetting-driven residuals
/// are a few percent, so this is an order of magnitude of headroom, not a
/// working limit.
///
/// It lives here, beside [`a_grid`], and not next to the other
/// local-scale constants in [`scale`] (review m5): the band is a property
/// of the SAMPLED grid, which is this module's job — `scale` never sees a
/// grid, only the spline it hands over.
pub const LN_LOCAL_SCALE_MAX_DEVIATION: f64 = 0.25;

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

/// One channel's `A` grid (ruling R-M4c-8): the global `scale` at every
/// node, or — when `local` carries the local scale spline
/// [`scale::fit_local_scale`] produced — that spline sampled at each
/// node's OWN pixel, `A(i, j) = scale + spline(i·stride, j·stride).0`.
/// Node `(i, j)` sits at `(i·stride, j·stride)` ([`LnGrid`]'s own mesh
/// convention), which is where the `B` term and the band loop's B-spline
/// both read it.
///
/// The trailing node on each axis overshoots the plane by design
/// (`LnGrid::node_count` makes the mesh REACH the last pixel, so its last
/// address can sit up to `stride − 1` past it) and is **clamped into the
/// plane before the spline is evaluated** — the identical rule
/// [`background::background_grid`] applies to that same node's own cell
/// window, and the one `examples/ln_probe.rs` reads the mesh back with
/// (review m7). `A` and `B` are therefore measured at the same pixel at
/// every node, including the trailing one, and the spline is never asked
/// for a position its own node cloud could not have covered.
///
/// The returned flag says whether the spline was actually used. It is
/// `false` for `local: None` and ALSO when the sampled surface left the
/// safety band [`LN_LOCAL_SCALE_MAX_DEVIATION`] defines around `scale` (or
/// produced a non-finite node): such a surface is refused as a whole — a
/// partially-clamped `A` grid would be a quiet lie about what was measured
/// — and the channel keeps the constant `A = scale`, which the caller
/// logs.
///
/// With no spline the grid is `vec![scale as f32; gw·gh]`, bit-for-bit
/// what M2/M3/M4a wrote, which is what keeps every LN pin passing while
/// `localScale` is off.
#[allow(clippy::too_many_arguments)]
fn a_grid(
    local: Option<&ThinPlateSpline>,
    scale: f64,
    gw: usize,
    gh: usize,
    stride: usize,
    ref_width: usize,
    ref_height: usize,
) -> (Vec<f32>, bool) {
    let constant = || (vec![scale as f32; gw * gh], false);
    let Some(spline) = local else {
        return constant();
    };
    let band = LN_LOCAL_SCALE_MAX_DEVIATION * scale.abs();
    let mut a = Vec::with_capacity(gw * gh);
    for j in 0..gh {
        let y = (j * stride).min(ref_height.saturating_sub(1)) as f64;
        for i in 0..gw {
            let x = (i * stride).min(ref_width.saturating_sub(1)) as f64;
            let v = scale + spline.displacement(x, y).0;
            if !v.is_finite() || (v - scale).abs() > band {
                return constant();
            }
            a.push(v as f32);
        }
    }
    (a, true)
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
///
/// `prepared[p]` (final fix wave, I2) is channel `p`'s
/// [`scale::PreparedReferenceChannel`] — the reference-side star detection
/// + PSF fit + match tree `scale::relative_scale` used to redo on EVERY
/// `normalize_frame` call, hoisted here for the same reason
/// `sanitized_planes`/`locations` already are: the reference is immutable
/// for the whole group, so this is built once and read by every frame's
/// [`scale::relative_scale_against`] call instead.
///
/// `sanitized_planes[p]` (M4a Task 5) borrows `reference`'s own plane
/// (`Cow::Borrowed`) instead of unconditionally cloning it — the common
/// case, since an all-finite reference plane (the overwhelming majority)
/// needs no sanitizing at all. Only a plane that actually carries a
/// non-finite pixel pays for an owned copy (`Cow::Owned`), exactly as
/// before. This is why the struct now carries a lifetime tied to the
/// `&'a LnReference` [`Self::build`] borrows from.
pub struct LnReferenceForDetection<'a> {
    pub sanitized_planes: Vec<Cow<'a, [f32]>>,
    pub locations: Vec<f64>,
    pub prepared: Vec<scale::PreparedReferenceChannel>,
}

impl<'a> LnReferenceForDetection<'a> {
    /// `psf`/`max_stars` are the group's own LN config — the SAME values
    /// [`normalize_frame`]'s own `relative_scale_against` calls use, so the
    /// prepared reference channel matches what a direct (unhoisted)
    /// `relative_scale` call on this reference would have produced.
    pub fn build(
        reference: &'a LnReference,
        psf: crate::stacking::psf_signal::PsfModel,
        max_stars: usize,
    ) -> LnReferenceForDetection<'a> {
        let mut sanitized_planes = Vec::with_capacity(reference.planes.len());
        let mut locations = Vec::with_capacity(reference.planes.len());
        for plane in &reference.planes {
            let location = median_of_finite(plane);
            let sanitized: Cow<'a, [f32]> = if plane.iter().all(|v| v.is_finite()) {
                Cow::Borrowed(plane.as_slice())
            } else {
                Cow::Owned(
                    plane
                        .iter()
                        .map(|&v| if v.is_finite() { v } else { location as f32 })
                        .collect(),
                )
            };
            sanitized_planes.push(sanitized);
            locations.push(location);
        }
        let prepared = sanitized_planes
            .iter()
            .map(|plane| {
                scale::PreparedReferenceChannel::build(
                    plane,
                    reference.width,
                    reference.height,
                    psf,
                    max_stars,
                )
            })
            .collect();
        LnReferenceForDetection {
            sanitized_planes,
            locations,
            prepared,
        }
    }
}

/// Warps `frame` into the reference geometry (one-frame [`RegisteredSource`],
/// whole plane per channel — a single band the height of the reference,
/// read once through the [`FrameSource`] trait exactly like every other
/// registered-frame consumer), models both backgrounds, takes the PSF scale
/// against the SAME channel of `reference`, builds `A = s`,
/// `B = B_ref − A·B_tgt` on the stride grid (spec §5.2/math §4.4) and writes
/// `<stem>.athln` at `sidecar` ([`LnFrameGrids::write`] — tmp file + atomic
/// rename, so a reader never observes a half-written sidecar).
///
/// With `cfg.local_scale` on (ruling R-M4c-8) `A` is no longer that one
/// number: [`scale::relative_scale_against`] also fits a thin-plate spline
/// through the matched stars' scale residuals and [`a_grid`] samples it at
/// every node, so `A` varies smoothly over the frame. Nothing else about
/// the sidecar changes — the format has always carried a full `A` grid —
/// and with the flag off every value written is what M2/M3/M4a wrote.
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
    reference_for_detection: &LnReferenceForDetection<'_>,
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
        || reference_for_detection.prepared.len() != channels
    {
        return Err(LnError::Other(format!(
            "reference-for-detection has {}/{}/{} channels, the LN reference has {channels}",
            reference_for_detection.sanitized_planes.len(),
            reference_for_detection.locations.len(),
            reference_for_detection.prepared.len()
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

        // I2 (final fix wave): the reference side (detection + PSF fit +
        // match tree) was already prepared ONCE for the whole group by
        // `LnReferenceForDetection::build` — `relative_scale_against` only
        // re-detects/re-fits the TARGET, not the reference, on every call.
        let scale_result = scale::relative_scale_against(
            &reference_for_detection.prepared[p],
            &target,
            reference.width,
            reference.height,
            measure.max_stars,
            4.0,
            0.3,
            cfg.local_scale,
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
        // M4c: the mesh the `A` grid is built on is a third party to this
        // check now (`a_grid` sizes itself from `expected_gw`/`expected_gh`
        // and `b` is zipped against it), so the release-build check covers
        // all three lengths rather than just reference-vs-target — a short
        // `b` would otherwise be a silently corrupt grid.
        let expected_cells = expected_gw * expected_gh;
        if ref_bg.cells.len() != target_bg.cells.len() || ref_bg.cells.len() != expected_cells {
            return Err(LnError::Other(format!(
                "background grid size mismatch: reference has {} cells, target has {}, the stride mesh has {expected_cells}",
                ref_bg.cells.len(),
                target_bg.cells.len()
            )));
        }
        // M4c ruling R-M4c-8: `A` is the global scale at every node unless
        // a local scale spline was fitted for this channel, in which case
        // it is that spline sampled node by node. `B = B_ref − A·B_tgt`
        // uses THIS node's own `A` either way — with no spline every
        // `a[k]` is the same `scale as f32` the M2 code multiplied by, so
        // the `b` cells come out bit-identical.
        let (a, local_used) = a_grid(
            scale_result.local.as_ref(),
            scale_result.scale,
            expected_gw,
            expected_gh,
            stride,
            reference.width,
            reference.height,
        );
        if let Some(spline) = scale_result.local.as_ref() {
            if local_used {
                tracing::debug!(
                    ln_scale = scale_result.scale,
                    ln_local_nodes = spline.nodes.len(),
                    "local scale: A sampled from the spline"
                );
            } else {
                tracing::warn!(
                    ln_scale = scale_result.scale,
                    ln_local_nodes = spline.nodes.len(),
                    "local scale: the sampled A grid left the safety band around the global scale; A stays the global scale"
                );
            }
        }
        let b: Vec<f32> = ref_bg
            .cells
            .iter()
            .zip(target_bg.cells.iter())
            .zip(a.iter())
            .map(|((&br, &bt), &ak)| br - ak * bt)
            .collect();

        grids.push(LnGrid {
            ref_width: reference.width,
            ref_height: reference.height,
            scale: cfg.scale,
            gw: expected_gw,
            gh: expected_gh,
            a,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stacking::psf_signal::PsfModel;

    /// Small enough that `register::detect::detect_stars` short-circuits
    /// (`w < 8 || h < 8`) to an empty result deterministically, so
    /// `LnReferenceForDetection::build`'s detect+fit path never has to find
    /// a real star for these tests — they check the `Cow` sanitizing
    /// behaviour only.
    fn reference_with_planes(planes: Vec<Vec<f32>>) -> LnReference {
        LnReference {
            width: 4,
            height: 3,
            planes,
            frames_used: Vec::new(),
        }
    }

    #[test]
    fn an_all_finite_reference_borrows_every_plane() {
        let planes = vec![
            vec![1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0],
            vec![0.5f32; 12],
        ];
        let reference = reference_with_planes(planes);
        let for_detection = LnReferenceForDetection::build(&reference, PsfModel::default(), 50);
        assert_eq!(for_detection.sanitized_planes.len(), 2);
        for (p, plane) in for_detection.sanitized_planes.iter().enumerate() {
            assert!(
                matches!(plane, Cow::Borrowed(_)),
                "channel {p}: expected a borrowed plane, got an owned copy"
            );
        }
    }

    #[test]
    fn a_plane_with_a_nan_is_sanitized_into_an_owned_copy() {
        let mut plane = vec![1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0];
        plane[5] = f32::NAN;
        let reference = reference_with_planes(vec![plane.clone()]);
        let location = median_of_finite(&plane);

        let for_detection = LnReferenceForDetection::build(&reference, PsfModel::default(), 50);
        assert_eq!(for_detection.sanitized_planes.len(), 1);
        match &for_detection.sanitized_planes[0] {
            Cow::Owned(v) => {
                assert!(v.iter().all(|x| x.is_finite()), "sanitized plane must be all-finite");
                assert!(
                    (v[5] as f64 - location).abs() < 1e-9,
                    "the NaN must be replaced by the plane's own finite median: {} vs {location}",
                    v[5]
                );
            }
            Cow::Borrowed(_) => panic!("a plane with a NaN must not be borrowed as-is"),
        }
    }

    // ---- M4c ruling R-M4c-8: the `A` grid from the local scale spline --

    const GRID_W: usize = 1024;
    const GRID_H: usize = 768;
    const GRID_STRIDE: usize = 128;
    /// The global scale the tests below build their surface around — a
    /// realistic relative scale, not 1.0, so a bug that returns the
    /// residual instead of `scale + residual` cannot pass.
    const TEST_SCALE: f64 = 0.835;

    /// A linear scale residual across the frame, the shape a flat-field
    /// residual has: `±0.0175` at the two edges, so `A` runs from
    /// `0.8175` to `0.8525` — a 4 % swing, well inside the safety band.
    fn linear_residual(x: f64) -> f64 {
        0.035 * (x / GRID_W as f64 - 0.5)
    }

    /// A thin-plate spline fitted (interpolating, λ = 0) on a 5×4 node
    /// grid carrying [`linear_residual`] as its x channel and zeros as its
    /// y channel — exactly the shape `scale::fit_local_scale` produces.
    fn linear_residual_spline(amplify: f64) -> ThinPlateSpline {
        let mut nodes = Vec::new();
        let mut dx = Vec::new();
        for j in 0..4 {
            for i in 0..5 {
                let x = 60.0 + i as f64 * 220.0;
                let y = 50.0 + j as f64 * 220.0;
                nodes.push((x, y));
                dx.push(amplify * linear_residual(x));
            }
        }
        let dy = vec![0.0f64; nodes.len()];
        ThinPlateSpline::fit(&nodes, &dx, &dy, 0.0)
            .expect("a 5x4 node grid with a linear x channel must be fittable")
    }

    #[test]
    fn a_grid_without_a_spline_is_the_constant_global_scale() {
        let (gw, gh) = LnGrid::grid_dims(GRID_W, GRID_H, GRID_STRIDE);
        let (a, used) = a_grid(None, TEST_SCALE, gw, gh, GRID_STRIDE, GRID_W, GRID_H);
        assert!(!used, "no spline means no local scale");
        assert_eq!(a.len(), gw * gh);
        assert!(
            a.iter().all(|&v| v == TEST_SCALE as f32),
            "every node must be the constant RCR location"
        );
    }

    /// The brief's own acceptance for the local scale: the sampled `A`
    /// grid follows the gradient at the grid's left, centre and right
    /// columns, within 0.01.
    #[test]
    fn a_grid_follows_the_spline_at_the_left_centre_and_right_columns() {
        let (gw, gh) = LnGrid::grid_dims(GRID_W, GRID_H, GRID_STRIDE);
        let spline = linear_residual_spline(1.0);
        let (a, used) = a_grid(
            Some(&spline),
            TEST_SCALE,
            gw,
            gh,
            GRID_STRIDE,
            GRID_W,
            GRID_H,
        );
        assert!(used, "the sampled surface is well inside the safety band");

        let row = gh / 2;
        for i in [0usize, gw / 2, gw - 1] {
            let x = (i * GRID_STRIDE) as f64;
            let got = a[row * gw + i] as f64;
            let want = TEST_SCALE + linear_residual(x);
            assert!(
                (got - want).abs() < 0.01,
                "node column {i} (x = {x}): A = {got}, expected {want} within 0.01"
            );
        }
        // And it is genuinely a GRADIENT, not a constant that happens to
        // sit near the middle of it.
        let left = a[row * gw] as f64;
        let right = a[row * gw + gw - 1] as f64;
        assert!(
            right - left > 0.02,
            "left {left} to right {right} — the grid did not follow the gradient"
        );
    }

    /// A surface that leaves the safety band is refused AS A WHOLE: the
    /// channel keeps the constant `A`, never a partially-clamped grid.
    #[test]
    fn a_sampled_surface_outside_the_safety_band_falls_back_to_the_constant() {
        let (gw, gh) = LnGrid::grid_dims(GRID_W, GRID_H, GRID_STRIDE);
        // 30x the residual above puts the edges at ±0.525 of a 0.835
        // scale, far past `LN_LOCAL_SCALE_MAX_DEVIATION` (0.25).
        let spline = linear_residual_spline(30.0);
        let (a, used) = a_grid(
            Some(&spline),
            TEST_SCALE,
            gw,
            gh,
            GRID_STRIDE,
            GRID_W,
            GRID_H,
        );
        assert!(!used, "the surface left the band and must be refused");
        assert!(
            a.iter().all(|&v| v == TEST_SCALE as f32),
            "the fallback must be the constant global scale, not a clamp"
        );
    }
}

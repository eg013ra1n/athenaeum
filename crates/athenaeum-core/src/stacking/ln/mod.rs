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
//! own was modelled — by the same procedure and thresholds (L-1), then both
//! surfaces are refitted on the samples both kept (L-6) — takes the PSF relative scale
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
use std::sync::Arc;
use std::time::Instant;

use crate::geometry::ThinPlateSpline;
use crate::integration::banded::BandPlanes;
use crate::integration::registered_source::{MaterializedFrame, RegisteredFrame, RegisteredSource};
use crate::integration::source::FrameSource;
use crate::integration::stats::median_in_place;
use crate::integration::IntegrationError;
use crate::resample::Interpolation;
use crate::stacking::integrate::{LocalNormalizationConfig, StackFrame};
use crate::stacking::measure::MeasureOptions;
use crate::stacking::psf_signal::StarFit;

pub mod background;
pub mod calibration;
pub mod grid;
pub mod reference;
pub mod scale;
pub mod scale_rule;
pub mod selection;

pub use scale_rule::{
    effective_scale_mode, resolve_scale, snap_ln_scale, LEGACY_DEFAULT_LN_SCALE, LN_SCALES,
    MAX_GRID_SIZE, MIN_GRID_SIZE,
};
pub use background::{
    shared_refit, BackgroundModel, BackgroundRefusal, CleaningParams, PairSurfaces, Surface,
    TileNoise, TileStat, TileTable, CLEANING,
};
pub use selection::{rank_candidates, Candidates, SelectionRule, SelectionSide};
pub use calibration::{
    calibration_k, measure_seeds_calibration, select_calibration_frames, CalibrationMeasurement,
    ChannelCalibration, ChannelOutcome, SeedsCalibration, SEEDS_CALIBRATION_BAND,
    SEEDS_CALIBRATION_FRAMES, SEEDS_CALIBRATION_MIN_RATIOS,
};
pub use grid::{LnFrameGrids, LnGrid};
pub use reference::{build_reference, read_reference, write_reference, LnReference};
pub use scale::{
    relative_scale, relative_scale_against, relative_scale_against_with_diag,
    relative_scale_from_seeds, relative_scale_from_seeds_with_breakdown,
    relative_scale_from_seeds_with_diag, PreparedReferenceChannel, ScaleInputs, ScaleMatchDiag, ScaleResult,
    SeedFilterBreakdown, LN_BARYCENTRE_PASS_THRESHOLD, LN_LOCAL_SCALE_MIN_STARS,
    LN_LOCAL_SCALE_SMOOTHING_SIGMAS,
};

/// The LN model's own version (spec 2026-10-04 §7, L-19), folded into the
/// normalization subtree so a change in what an existing config produces
/// moves the `ln`, `ln_reference` and `ln_calibration` hashes together.
/// 1 = the cell model (implicit, never written); 2 = the background model
/// at the scale; 3 = the star selection (spec §3); bump it again whenever
/// an existing config's output moves.
pub const LN_MODEL_VERSION: u32 = 3;

/// What [`normalize_frame`] is given to seed the relative-scale step with
/// (perf tier C Task 2 fix round 4, ruling C-14 item 3).
///
/// The distinction that matters is between the two ways a frame ends up on
/// the full-detection path: [`Self::NoFits`] is the GENUINE fallback (this
/// frame's Measure artifact is missing or unusable — one `warn!`, counted),
/// while [`Self::ForcedDetection`] is the seeds calibration asking for that
/// path ON PURPOSE, as one arm of its own measurement. Before this
/// distinction existed the calibration's three detection runs per group
/// fired the fallback `warn!` and bumped the fallback counter, which made
/// both the log line and the run pin that counts fallbacks lie (the review's
/// I1/I2).
pub enum LnScaleSeeds<'a> {
    /// Measure's accepted [`StarFit`]s, one list per plane. A plane with
    /// fewer than [`scale::MIN_MATCHES`] entries — or whose
    /// [`scale::relative_scale_from_seeds`] call itself comes back
    /// `TooFewMatches` — falls back to detection exactly like
    /// [`Self::NoFits`], warning and counting once for the whole frame.
    Measured(&'a [Vec<StarFit>]),
    /// No fits at all for this frame (an old catalog, a cleanup): the
    /// genuine fallback.
    NoFits,
    /// The seeds calibration's own detection arm: full detection, silent,
    /// never counted as a fallback.
    ForcedDetection,
}

impl LnScaleSeeds<'_> {
    /// This plane's usable seed fits, or `None` when the plane must take
    /// the detection path.
    fn plane_fits(&self, plane: usize) -> Option<&[StarFit]> {
        match self {
            LnScaleSeeds::Measured(fits) => fits
                .get(plane)
                .filter(|pf| pf.len() >= scale::MIN_MATCHES)
                .map(|pf| pf.as_slice()),
            LnScaleSeeds::NoFits | LnScaleSeeds::ForcedDetection => None,
        }
    }

    /// Whether a detection run under this input is a FALLBACK (loud,
    /// counted) rather than the point of the call.
    fn detection_is_a_fallback(&self) -> bool {
        !matches!(self, LnScaleSeeds::ForcedDetection)
    }
}

/// Perf tier C Task 2: how many times [`normalize_frame`] fell back to full
/// detection (`scale::relative_scale_against`) because a plane had no
/// usable `fits` artifact or [`scale::relative_scale_from_seeds`] itself
/// came back `TooFewMatches` — one count per FRAME (a multi-channel OSC
/// frame that falls back on any one of its planes counts once), mirroring
/// [`crate::integration::registered_source::fallback_counters`]'s own
/// shape and reasoning: a test asserting the specific `warn!` line would
/// need to own a global tracing subscriber exclusively, which this test
/// binary's parallel run cannot guarantee (Task 6a Pin 3's own doc).
#[cfg(test)]
pub(crate) mod fallback_counters {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, MutexGuard};

    pub static FALLBACKS: AtomicUsize = AtomicUsize::new(0);

    /// Held by every test that measures the fallback counter.
    static EXCLUSIVE: Mutex<()> = Mutex::new(());

    pub fn exclusive() -> MutexGuard<'static, ()> {
        EXCLUSIVE.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(super) fn on_fallback() {
        FALLBACKS.fetch_add(1, Ordering::Relaxed);
    }

    pub fn reset() {
        FALLBACKS.store(0, Ordering::Relaxed);
    }
}

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

/// How a group's LN fits treat what a member and the LN reference do not
/// both cover (spec §5.6, plan 6b-3 D6). `Legacy` — every frame set —
/// sanitizes non-finite pixels to the plane's median for detection, exactly
/// as before. `Masked` — a project on its grid — fits each member on its
/// overlap with the reference: detection planes fill the rest with 0.0
/// (the fast detector leaves values <= 0 out of its global background
/// histogram and exact zeros out of its global RMS; its local steps still
/// see them, which is why seeds near the edge are dropped), seeds within [`LN_MASK_MARGIN_PX`] of the
/// overlap's edge are dropped, and both backgrounds are modelled on the
/// overlap only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LnOverlap {
    Legacy,
    Masked,
}

/// Spec §5.6: a member whose overlap with the LN reference is under this
/// fraction of its own footprint keeps global normalization (plan 6b-3 D7).
pub const LN_MIN_OVERLAP: f64 = 0.25;

/// The PSF fit region's hard cap (`psf_signal::fit_one`, `.min(48)`): a
/// seed this far inside the overlap fits on overlap pixels only.
pub const LN_MASK_MARGIN_PX: usize = 48;

/// `valid` eroded by `margin` (Chebyshev): a pixel stays valid when every
/// pixel within `margin` of it, inside the image, is valid. Two separable
/// passes over prefix sums, `O(width * height)`.
pub fn eroded_mask(valid: &[bool], width: usize, height: usize, margin: usize) -> Vec<bool> {
    let mut rows = vec![false; valid.len()];
    let mut prefix = vec![0u32; width.max(height) + 1];
    for y in 0..height {
        let row = &valid[y * width..(y + 1) * width];
        for x in 0..width {
            prefix[x + 1] = prefix[x] + (!row[x]) as u32;
        }
        for x in 0..width {
            let (a, b) = (x.saturating_sub(margin), (x + margin + 1).min(width));
            rows[y * width + x] = prefix[b] - prefix[a] == 0;
        }
    }
    let mut out = vec![false; valid.len()];
    for x in 0..width {
        for y in 0..height {
            prefix[y + 1] = prefix[y] + (!rows[y * width + x]) as u32;
        }
        for y in 0..height {
            let (a, b) = (y.saturating_sub(margin), (y + margin + 1).min(height));
            out[y * width + x] = prefix[b] - prefix[a] == 0;
        }
    }
    out
}

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
    /// Plan 6b-3 D7 (spec §5.6): the member overlaps the LN reference on
    /// less than [`LN_MIN_OVERLAP`] of its own footprint (channel 0). The
    /// run keeps it on its global normalization; it is never an exclusion.
    LowOverlap {
        fraction: f64,
    },
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
            LnError::LowOverlap { fraction } => write!(
                f,
                "local normalization: the frame overlaps the LN reference on {:.0} % of its footprint (< {:.0} %)",
                fraction * 100.0,
                LN_MIN_OVERLAP * 100.0
            ),
        }
    }
}

impl std::error::Error for LnError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LnError::Io(e) => Some(e),
            LnError::TooFewMatches { .. } | LnError::Other(_) | LnError::LowOverlap { .. } => None,
        }
    }
}

/// One frame's stage-6 outcome (M2 Task 5): where its sidecar landed, the
/// frame-level scale/matches/background-sample counts the run logs and reports in its
/// provenance (spec §9.3/§9.5) — averaged/summed across channels when the
/// frame carries more than one (a debayered OSC light): `scale` is the mean
/// of every channel's own [`scale::ScaleResult::scale`] (a single number for
/// `SummaryFrame.ln_scale`/the `FramesTable` UI column; each channel's own
/// value is still exactly what its own [`LnGrid::global_scale`] carries),
/// `matches` and the four background counters are the SUM over channels
/// (how much data backed the whole frame's normalization, not just one
/// channel's).
#[derive(Debug, Clone, Default)]
pub struct LnFrameOutcome {
    pub sidecar: std::path::PathBuf,
    pub scale: f64,
    pub matches: usize,
    // Background samples of the frame's own final fit, the samples its
    // rejection rounds left out, the samples of the shared refit, and the
    // knot cells under the floor — each summed over channels (spec §10).
    pub samples: usize,
    pub samples_rejected: usize,
    pub samples_shared: usize,
    pub cells_unsupported: usize,
    /// Wall time (ms) spent warping into the reference geometry, summed
    /// across every channel (perf tier 1 Task 0).
    pub warp_ms: u64,
    /// Wall time (ms) spent on the target's own background model, summed
    /// across every channel.
    pub background_ms: u64,
    /// Wall time (ms) spent measuring the PSF-flux relative scale, summed
    /// across every channel.
    pub scale_ms: u64,
    /// Wall time (ms) writing the `.athln` sidecar — once for the whole
    /// frame, not per channel.
    pub write_ms: u64,
    /// Of `scale_ms`, how much was the target's own star detection inside
    /// `scale::relative_scale_against` (perf tier A Task 0, audit §3.1) —
    /// summed across every channel, same contract as `scale_ms` itself.
    pub detect_ms: u64,
    /// Reserved for the centroid-refine LM step Task 3 splits out of
    /// detection; always `0` until then.
    pub refine_ms: u64,
    /// Of `scale_ms`, how much was the target's PSF fit at the reference's
    /// resolved β, summed across every channel.
    pub fit_ms: u64,
    /// Of `scale_ms`, how much was matching + RCR (and, when on, the local
    /// scale spline), summed across every channel.
    pub match_ms: u64,
    /// Perf tier C Task 2: `"seeds"` when every channel's relative scale
    /// came from Measure's persisted fits used as seed positions
    /// ([`scale::relative_scale_from_seeds`]); `"detected"` when at least
    /// one channel fell back to full detection
    /// ([`scale::relative_scale_against`]) — a frame's own log/summary is
    /// one value, so a mixed OSC frame reads as the worst case. Named
    /// `ln_scale_source` (fix round 4, ruling C-14 item 4) after the
    /// logging dictionary's own `ln_` prefix rule, and to keep it clearly
    /// apart from `stacking::groups::GroupFrame::scale_source`, which is
    /// M4b's PIXEL-scale provenance and an unrelated thing.
    pub ln_scale_source: &'static str,
    /// Each channel's own final relative scale — the constant term of that
    /// channel's `A`, with the group's seeds calibration already applied
    /// (`scale` above is their mean). Indexed like the LN reference's
    /// planes.
    pub channel_scales: Vec<f64>,
    /// Per channel, whether its scale came from the seeds path (`true`) or
    /// from full detection (`false`). The seeds calibration reads this to
    /// decide which channels contribute a ratio — a channel that fell back
    /// compared detection against detection and has nothing to say about
    /// the seeds path's own bias.
    pub channel_from_seeds: Vec<bool>,
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
    let mut finite: Vec<f32> = plane.iter().copied().filter(|v| v.is_finite()).collect();
    if finite.is_empty() {
        return f64::NAN;
    }
    // Perf tier A Task 12: `median_in_place` directly on the owned `finite`
    // buffer instead of `median_of`, which itself clones its argument
    // before calling `median_in_place` — this function used to pay for
    // TWO copies of the finite pixels (the `filter().collect()` above, then
    // `median_of`'s own internal `.to_vec()`) for one median. Selection is
    // order-independent (the k-th order statistic of a multiset does not
    // depend on the array's arrangement), so mutating `finite`'s order in
    // place changes nothing here — nothing reads `finite` again afterward.
    median_in_place(&mut finite) as f64
}

/// One channel's `A` grid (ruling R-M4c-8): the global `scale` at every
/// node, or — when `local` carries the local scale spline
/// [`scale::fit_local_scale`] produced — that spline sampled at each
/// node's OWN pixel, `A(i, j) = k · (scale + spline(i·stride, j·stride).0)`.
/// Node `(i, j)` sits at `(i·stride, j·stride)` ([`LnGrid`]'s own mesh
/// convention), which is where the `B` term and the band loop's B-spline
/// both read it.
///
/// `k` is the group's seeds calibration for THIS channel (fix round 4,
/// ruling C-14 item 5 — the review's M6), `1.0` for a channel that took
/// the detection path or a group with no calibration. It scales the
/// SAMPLED surface, not just its constant term: fix round 3 skipped
/// calibration on a channel carrying a spline, reasoning that the spline's
/// residuals had been fitted against an uncalibrated `s` — but the spline
/// is a residual AROUND `scale`, so scaling the whole sampled value
/// (`k·scale + k·displacement`) keeps the two consistent by construction,
/// and leaving such a channel uncalibrated was the larger inconsistency
/// (its `A` would sit a measured fraction of a percent away from every
/// other channel's). The safety band is checked on the UNSCALED deviation
/// `|displacement|` against `LN_LOCAL_SCALE_MAX_DEVIATION · |scale|`, which
/// for `k > 0` is the identical test as checking the scaled surface against
/// `k·scale` — so no surface's accept/refuse verdict moves because `k`
/// exists.
///
/// The trailing node on each axis overshoots the plane by design
/// (`LnGrid::node_count` makes the mesh REACH the last pixel, so its last
/// address can sit up to `stride − 1` past it) and is **clamped into the
/// plane before the spline is evaluated** — the identical rule
/// [`background::Surface::nodes`] applies to that same node, and the one `examples/ln_probe.rs` reads the mesh back with
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
    k: f64,
    gw: usize,
    gh: usize,
    stride: usize,
    ref_width: usize,
    ref_height: usize,
) -> (Vec<f32>, bool) {
    let constant = || (vec![(scale * k) as f32; gw * gh], false);
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
            a.push((v * k) as f32);
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
/// The median sanitizing described here applies to [`LnOverlap::Legacy`]
/// only; [`Self::build_masked`] fills non-finite pixels with 0.0.
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
    pub overlap: LnOverlap,
}

impl<'a> LnReferenceForDetection<'a> {
    /// `psf`/`max_stars` are the group's own LN config — the SAME values
    /// [`normalize_frame`]'s own `relative_scale_against` calls use, so the
    /// prepared reference channel's beta matches what a direct (unhoisted)
    /// `relative_scale` call on this reference would have produced.
    /// (Ruling C-10's per-β reference fits, retired by ruling C-12 and
    /// removed outright in fix round 4 — the review's M2 — used to take an
    /// `extra_betas` slice here; every caller passed an empty one, since
    /// [`scale::relative_scale_from_seeds`] always fits at the reference's
    /// own beta regardless of what beta the seed source's fits carry.)
    /// `pool` (perf tier 1 Task 2 fix round 1, item 3) is threaded straight
    /// to [`scale::PreparedReferenceChannel::build`] — this runs once per
    /// group, not once per frame.
    pub fn build(
        reference: &'a LnReference,
        psf: crate::stacking::psf_signal::PsfModel,
        max_stars: usize,
        pool: Option<&Arc<rayon::ThreadPool>>,
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
                    pool,
                )
            })
            .collect();
        LnReferenceForDetection {
            sanitized_planes,
            locations,
            prepared,
            overlap: LnOverlap::Legacy,
        }
    }

    /// The `Masked` preparation (plan 6b-3 D6): non-finite reference pixels
    /// — the grid no member covers — become 0.0 for detection, never the
    /// median, and the reference's own stars within [`LN_MASK_MARGIN_PX`]
    /// of its coverage edge are dropped before they are fitted.
    pub fn build_masked(
        reference: &'a LnReference,
        psf: crate::stacking::psf_signal::PsfModel,
        max_stars: usize,
        pool: Option<&Arc<rayon::ThreadPool>>,
    ) -> LnReferenceForDetection<'a> {
        let mut sanitized_planes = Vec::with_capacity(reference.planes.len());
        let mut locations = Vec::with_capacity(reference.planes.len());
        let mut prepared = Vec::with_capacity(reference.planes.len());
        for plane in &reference.planes {
            let finite: Vec<bool> = plane.iter().map(|v| v.is_finite()).collect();
            let sanitized: Cow<'a, [f32]> = if finite.iter().all(|&f| f) {
                Cow::Borrowed(plane.as_slice())
            } else {
                Cow::Owned(
                    plane
                        .iter()
                        .map(|&v| if v.is_finite() { v } else { 0.0 })
                        .collect(),
                )
            };
            let allowed = eroded_mask(
                &finite,
                reference.width,
                reference.height,
                LN_MASK_MARGIN_PX,
            );
            prepared.push(scale::PreparedReferenceChannel::build_masked(
                &sanitized,
                reference.width,
                reference.height,
                psf,
                max_stars,
                pool,
                &allowed,
            ));
            locations.push(median_of_finite(plane));
            sanitized_planes.push(sanitized);
        }
        LnReferenceForDetection {
            sanitized_planes,
            locations,
            prepared,
            overlap: LnOverlap::Masked,
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
///
/// **Perf tier C Task 2** (spec §2.2.3, fix round 2 ruling C-12): `seeds`
/// carries Measure's own accepted [`StarFit`]s for this frame, one list per
/// plane ([`LnScaleSeeds::Measured`] — a plane with fewer than
/// [`scale::MIN_MATCHES`] entries is treated as if there were no fits at
/// all), used
/// as SEED POSITIONS ONLY — mapped through `frame.map`, pre-selected
/// against the reference's own match tree, and re-fitted fresh on the
/// warped `target` plane at the reference's DEFAULT β
/// ([`scale::relative_scale_from_seeds`]) — instead of running a full
/// detection search there. The expensive full-frame
/// detection (`detect_ms`, most of `scale_ms` before this task)
/// disappears for a plane whose fits are usable; the PSF fit itself still
/// runs, now over a much smaller, pre-matched seed list. Ruling C-11's
/// diagnostics found the earlier design (comparing Measure's own fitted
/// flux, mapped and Jacobian-corrected, against the reference directly)
/// biased by up to ~20% on undersampled real frames — a Moffat fit's own
/// `signal` is not warp-invariant, so this task now discards Measure's
/// fitted VALUES entirely and keeps only the positions. A plane with no
/// usable fits, or whose `relative_scale_from_seeds` call itself comes
/// back [`LnError::TooFewMatches`], falls back to today's
/// [`scale::relative_scale_against`] on the warped `target` plane — never
/// a failure, and the master is still an honest LN master — with exactly
/// ONE `warn!` for the whole frame (not one per plane) the first time any
/// channel needs it, and none at all under
/// [`LnScaleSeeds::ForcedDetection`], which asks for that path on purpose.
/// There is no `group_beta` parameter: the reference's
/// β (ruling C-1) is already baked into
/// `reference_for_detection`'s own prepared channels by the caller
/// (`stacking::run`'s `LnReferenceForDetection::build` call), and the
/// seeds path never reads a target's own β at all.
///
/// **Fix round 4 (ruling C-14):** `seeds_calibration` is the group's own
/// per-CHANNEL factor — `ln::calibration`'s module doc has the measurement
/// — applied to a channel's `A` ONLY when that channel actually took the
/// seeds path (as opposed to falling back to
/// [`scale::relative_scale_against`]): the fallback path is uncalibrated by
/// definition (LEVER 1's own diagnostics found no bias to correct there —
/// only the seeds path's fitted-signal warp-dependence, ruling C-11). A
/// channel carrying a fitted local-scale spline is calibrated too, by
/// scaling the SAMPLED `A` surface rather than its constant term alone
/// (ruling C-14 item 5 — see [`a_grid`]); fix round 3 skipped such a
/// channel, which left it a measured fraction of a percent away from every
/// other channel of the same group. A shorter slice than the reference has
/// planes, or `None` (the group's calibration was cancelled or could not be
/// measured at all), reads as `k = 1` — every channel exactly as it was
/// before this fix round.
#[allow(clippy::too_many_arguments)]
pub fn normalize_frame(
    reference: &LnReference,
    reference_for_detection: &LnReferenceForDetection<'_>,
    ref_models: &[BackgroundModel],
    frame: &StackFrame,
    seeds: LnScaleSeeds<'_>,
    cfg: &LocalNormalizationConfig,
    scale_px: u32,
    measure: &MeasureOptions,
    interpolation: Interpolation,
    clamping: f32,
    sidecar: &Path,
    seeds_calibration: Option<&SeedsCalibration>,
    pool: Option<&Arc<rayon::ThreadPool>>,
    cancel: &AtomicBool,
) -> Result<LnFrameOutcome, LnError> {
    let channels = reference.planes.len();
    if frame.measurement.channels.len() != channels {
        return Err(LnError::Other(format!(
            "frame has {} measured channels, the LN reference has {channels}",
            frame.measurement.channels.len()
        )));
    }
    if ref_models.len() != channels {
        return Err(LnError::Other(format!(
            "{} reference background models for a {channels}-channel LN reference",
            ref_models.len()
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

    let stride = (scale_px / 8).max(2) as usize;

    let mut grids = Vec::with_capacity(channels);
    let mut scales = Vec::with_capacity(channels);
    let mut from_seeds_per_channel = Vec::with_capacity(channels);
    let mut matches_total = 0usize;
    let (mut samples_total, mut samples_rejected_total) = (0usize, 0usize);
    let (mut samples_shared_total, mut cells_unsupported_total) = (0usize, 0usize);
    let mut warned_no_shared = false;
    // Per-phase wall time, accumulated ACROSS channels — one number per
    // phase for the whole frame (perf tier 1 Task 0).
    let mut warp_ms = 0u64;
    let mut background_ms = 0u64;
    let mut scale_ms = 0u64;
    // Perf tier A Task 0: `scale_ms`'s own sub-phase split, same
    // accumulate-across-channels contract.
    let mut detect_ms = 0u64;
    let mut refine_ms = 0u64;
    let mut fit_ms = 0u64;
    let mut match_ms = 0u64;
    // Perf tier C Task 2: "seeds" only while every channel so far used
    // Measure's persisted fits as seed positions; the first channel that
    // falls back to detection flips this for the whole frame (a mixed OSC
    // frame reads as the worst case) and fires the ONE per-frame fallback
    // `warn!`.
    let mut frame_scale_source: &'static str = "seeds";
    let mut warned_fallback = false;

    // Perf tier 1 Task 8: one `RegisteredSource` per FRAME, re-pointed at
    // each channel with `set_plane` (ruling R-T4-7's own reasoning, applied
    // here) instead of a fresh `RegisteredSource::open` — one
    // `PlaneReader::open` and, under a TPS map, one displacement-grid build
    // per frame rather than one per channel (3x on an OSC frame). `band`
    // and `target` are likewise allocated once and reused across channels:
    // `target` is fully overwritten by `decode_frame_into` on every
    // iteration, so the in-place NaN sanitizing below (which mutates
    // `target` for detection, after the tile table/`median_of_finite`
    // have already read the NaN-preserving version) never leaks into the
    // next channel's warp.
    let t = Instant::now();
    // Perf tier A Task 6a: reads the frame's materialized registered
    // artifact (`frame.registered_path`, resolved once by `stacking::run`)
    // verbatim when it is fresh and usable, falling back to the on-the-fly
    // warp otherwise — see `RegisteredSource::open_materialized`'s own doc.
    let registered = MaterializedFrame {
        registered_path: frame.registered_path.clone(),
        frame_id: Some(frame.frame_id),
        fallback: RegisteredFrame {
            path: frame.path.clone(),
            map: frame.map.clone(),
        },
    };
    let mut src = RegisteredSource::open_materialized(
        &[registered],
        reference.width,
        reference.height,
        0,
        interpolation,
        clamping,
    )
    .map_err(|e| LnError::Other(format!("warping into the reference geometry: {e}")))?;
    if let Some(pl) = pool {
        src = src.with_pool(Arc::clone(pl));
    }
    let mut band = BandPlanes::new(&src);
    let mut target = vec![0f32; reference.width * reference.height];
    warp_ms += t.elapsed().as_millis() as u64;

    for p in 0..channels {
        // The span names the frame and plane for every event of this
        // iteration: scale.rs's "ln relative scale" debug event carries no
        // identity of its own (`frame_id` is the catalog id of a frame-set
        // frame, the `stack_project_frames` handle of a project frame).
        let _ln_span =
            tracing::debug_span!("ln_plane", frame_id = frame.frame_id, plane = p).entered();
        if cancel.load(Ordering::Relaxed) {
            return Err(LnError::Other("cancelled".to_string()));
        }

        let t = Instant::now();
        src.set_plane(p)
            .map_err(|e| LnError::Other(e.to_string()))?;
        let no_progress = |_: u64| {};
        src.read_band_with_progress(0, reference.height, &mut band, 1, &no_progress, cancel)
            .map_err(|e| match e {
                IntegrationError::Cancelled => LnError::Other("cancelled".to_string()),
                other => LnError::Other(format!("warping into the reference geometry: {other}")),
            })?;
        band.decode_frame_into(0, &mut target);
        warp_ms += t.elapsed().as_millis() as u64;

        // Plan 6b-3 D6 (spec §5.6 seam 2): on a grid, the member's models are
        // fitted on its overlap with the LN reference, never on sanitized NaN.
        // Nodes outside the overlap hold the flood-fill extension of both
        // overlap backgrounds (the surface continues over the masked tiles
        // by its membrane) and ARE read: integration samples the grid
        // through its 4x4 cubic B-spline, so nodes up to two strides
        // outside the overlap reach pixels inside it, and it also
        // normalizes member pixels the reference does not cover.
        let masked = reference_for_detection.overlap == LnOverlap::Masked;
        let mut member_ref_model: Option<BackgroundModel> = None;
        let mut allowed: Option<Vec<bool>> = None;
        if masked {
            let ref_plane = &reference.planes[p];
            let mut overlap = vec![false; target.len()];
            let (mut covered, mut both) = (0usize, 0usize);
            for (k, o) in overlap.iter_mut().enumerate() {
                let in_target = target[k].is_finite();
                covered += in_target as usize;
                *o = in_target && ref_plane[k].is_finite();
                both += *o as usize;
            }
            if p == 0 {
                let fraction = if covered == 0 {
                    0.0
                } else {
                    both as f64 / covered as f64
                };
                if fraction < LN_MIN_OVERLAP {
                    return Err(LnError::LowOverlap { fraction });
                }
            }
            for (v, &o) in target.iter_mut().zip(&overlap) {
                if !o {
                    *v = f32::NAN;
                }
            }
            let restricted = ref_models[p].table.restricted(
                &reference.planes[p],
                &overlap,
                &CLEANING,
                pool,
            );
            member_ref_model = Some(BackgroundModel::fit(restricted, scale_px).map_err(|r| {
                tracing::warn!(
                    frame_id = frame.frame_id,
                    plane = p,
                    error = %r,
                    "ln: the LN reference's background model on this frame's overlap is refused"
                );
                LnError::Other(format!(
                    "background model of the LN reference on the overlap: {r}"
                ))
            })?);
            allowed = Some(eroded_mask(
                &overlap,
                reference.width,
                reference.height,
                LN_MASK_MARGIN_PX,
            ));
        }

        let t = Instant::now();
        // Perf tier A Task 12: `pool` fans the tile measurement out across
        // workers instead of running one tile at a time on this fan-out
        // thread.
        let target_model = BackgroundModel::fit(
            TileTable::measure(&target, reference.width, reference.height, &CLEANING, pool),
            scale_px,
        )
        .map_err(|r| {
            tracing::warn!(
                frame_id = frame.frame_id,
                plane = p,
                error = %r,
                "ln: the frame's background model is refused"
            );
            LnError::Other(format!("background model: {r}"))
        })?;
        let ref_model = member_ref_model.as_ref().unwrap_or(&ref_models[p]);
        let pair = background::shared_refit(ref_model, &target_model);
        if !pair.refit && !warned_no_shared {
            tracing::warn!(
                frame_id = frame.frame_id,
                plane = p,
                "no background sample in common with the LN reference; own-threshold backgrounds kept"
            );
            warned_no_shared = true;
        }
        let (expected_gw, expected_gh) =
            LnGrid::grid_dims(reference.width, reference.height, stride);
        let ref_nodes = pair.reference.nodes(stride);
        let tgt_nodes = pair.target.nodes(stride);
        samples_total += target_model.samples;
        samples_rejected_total += target_model.samples_rejected;
        samples_shared_total += pair.shared;
        cells_unsupported_total += target_model.cells_unsupported;

        // Fix round 1, item 7: the median over FINITE pixels only — see
        // `median_of_finite`'s own doc for why a NaN-laden plane still needs
        // this even though `total_cmp`-based sorting never panics on NaN.
        let location_tgt = median_of_finite(&target);
        background_ms += t.elapsed().as_millis() as u64;

        // Fix round 1, item 6: sanitize `target` IN PLACE for detection —
        // everything that needed the NaN-preserving version
        // (the tile table, `location_tgt`) has already read it, so
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
        // `fill`: see [`LnOverlap`] for why Masked uses 0.0 and Legacy the median.
        let fill = if masked { 0.0f32 } else { location_tgt as f32 };
        if !target.iter().all(|v| v.is_finite()) {
            for v in target.iter_mut() {
                if !v.is_finite() {
                    *v = fill;
                }
            }
        }

        // I2 (final fix wave): the reference side (detection + PSF fit +
        // match tree) was already prepared ONCE for the whole group by
        // `LnReferenceForDetection::build` — `relative_scale_against` only
        // re-detects/re-fits the TARGET, not the reference, on every call.
        //
        // Perf tier C Task 2 (ruling C-12): a plane with a usable `fits`
        // list (Measure's own accepted stars, mapped through `frame.map`
        // and used as SEED POSITIONS ONLY) skips the expensive full-frame
        // detection but still runs the PSF fit itself, over that
        // pre-selected seed list (`scale::relative_scale_from_seeds`);
        // anything else — no fits for this plane, or that call itself
        // reporting too few matches after mapping/pre-selection — falls
        // back to the full-detection path on the warped `target` plane,
        // with exactly one `warn!` for the whole frame.
        // P-7: on a project member the reference side is the member's
        // restricted model (its plane maximum and tile sigma).
        let ref_model_for_scale = member_ref_model.as_ref().unwrap_or(&ref_models[p]);
        let (ref_noise, tgt_noise) = (
            TileNoise::from_model(ref_model_for_scale),
            TileNoise::from_model(&target_model),
        );
        let inputs = scale::ScaleInputs {
            selection: cfg.star_selection,
            reference: SelectionSide {
                plane: &reference_for_detection.sanitized_planes[p],
                width: reference.width,
                height: reference.height,
                noise: &ref_noise,
            },
            target: SelectionSide {
                plane: &target,
                width: reference.width,
                height: reference.height,
                noise: &tgt_noise,
            },
        };
        let t = Instant::now();
        let plane_fits = seeds.plane_fits(p);
        // Ruling C-14 item 3: only a GENUINE fallback is loud and counted.
        // The seeds calibration's own detection arm
        // (`LnScaleSeeds::ForcedDetection`) runs this same code because
        // detection IS what it asked for — warning there would report a
        // defect that did not happen, and counting it would make the run
        // pin that counts fallbacks pass no matter what.
        let detection_is_a_fallback = seeds.detection_is_a_fallback();
        let fall_back_to_detection = |warned_fallback: &mut bool| -> Result<ScaleResult, LnError> {
            if detection_is_a_fallback && !*warned_fallback {
                tracing::warn!(
                    frame_id = frame.frame_id,
                    path = %frame.path.display(),
                    "ln: no measured fits, detecting on the warped frame"
                );
                *warned_fallback = true;
                #[cfg(test)]
                fallback_counters::on_fallback();
            }
            match allowed.as_deref() {
                Some(a) => scale::relative_scale_against_masked(
                    &reference_for_detection.prepared[p],
                    &target,
                    reference.width,
                    reference.height,
                    measure.max_stars,
                    4.0,
                    0.3,
                    cfg.local_scale,
                    pool,
                    a,
                    &inputs,
                ),
                None => scale::relative_scale_against(
                    &reference_for_detection.prepared[p],
                    &target,
                    reference.width,
                    reference.height,
                    measure.max_stars,
                    4.0,
                    0.3,
                    cfg.local_scale,
                    pool,
                    &inputs,
                ),
            }
        };
        let mut plane_from_seeds = true;
        let scale_result = match plane_fits {
            Some(pf) => match match allowed.as_deref() {
                Some(a) => scale::relative_scale_from_seeds_masked(
                    &reference_for_detection.prepared[p],
                    pf,
                    &frame.map,
                    &target,
                    reference.width,
                    reference.height,
                    measure.max_stars,
                    4.0,
                    0.3,
                    cfg.local_scale,
                    pool,
                    a,
                    &inputs,
                ),
                None => scale::relative_scale_from_seeds(
                    &reference_for_detection.prepared[p],
                    pf,
                    &frame.map,
                    &target,
                    reference.width,
                    reference.height,
                    measure.max_stars,
                    4.0,
                    0.3,
                    cfg.local_scale,
                    pool,
                    &inputs,
                ),
            } {
                Ok(r) => r,
                Err(LnError::TooFewMatches { .. }) => {
                    plane_from_seeds = false;
                    frame_scale_source = "detected";
                    fall_back_to_detection(&mut warned_fallback)?
                }
                Err(e) => return Err(e),
            },
            None => {
                plane_from_seeds = false;
                frame_scale_source = "detected";
                fall_back_to_detection(&mut warned_fallback)?
            }
        };
        // Ruling C-14 item 1: the group's own per-CHANNEL calibration
        // factor, and only for a channel that actually took the seeds path
        // — the detection path is uncalibrated by definition (LEVER 1's
        // diagnostics found no bias there to correct). A channel carrying
        // a local-scale spline is calibrated too, by scaling the sampled
        // surface in `a_grid` (ruling C-14 item 5), so `k` never has to be
        // folded into `ScaleResult::scale` itself — which stays exactly
        // what the measurement produced.
        let k = if plane_from_seeds {
            // C-17: `SeedsCalibration::k_for` owns the short-slice and
            // non-finite guards, so the apply site never restates them.
            seeds_calibration.map_or(1.0, |c| c.k_for(p))
        } else {
            1.0
        };
        let channel_scale = scale_result.scale * k;
        scale_ms += t.elapsed().as_millis() as u64;
        detect_ms += scale_result.timings.detect_ms;
        refine_ms += scale_result.timings.refine_ms;
        fit_ms += scale_result.timings.fit_ms;
        match_ms += scale_result.timings.match_ms;
        matches_total += scale_result.matches;
        scales.push(channel_scale);
        from_seeds_per_channel.push(plane_from_seeds);

        // Fix round 1, item 8: a real length check, not just the mesh the
        // two surfaces were evaluated on. `zip` would otherwise silently
        // truncate to the shorter of the two in release builds.
        // M4c: the mesh the `A` grid is built on is a third party to this
        // check now (`a_grid` sizes itself from `expected_gw`/`expected_gh`
        // and `b` is zipped against it), so the release-build check covers
        // all three lengths rather than just reference-vs-target — a short
        // `b` would otherwise be a silently corrupt grid.
        let expected_cells = expected_gw * expected_gh;
        if ref_nodes.len() != tgt_nodes.len() || ref_nodes.len() != expected_cells {
            return Err(LnError::Other(format!(
                "background grid size mismatch: reference has {} nodes, target has {}, the stride mesh has {expected_cells}",
                ref_nodes.len(),
                tgt_nodes.len()
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
            k,
            expected_gw,
            expected_gh,
            stride,
            reference.width,
            reference.height,
        );
        if let Some(spline) = scale_result.local.as_ref() {
            if local_used {
                tracing::debug!(
                    ln_scale = channel_scale,
                    ln_local_nodes = spline.nodes.len(),
                    "local scale: A sampled from the spline"
                );
            } else {
                tracing::warn!(
                    ln_scale = channel_scale,
                    ln_local_nodes = spline.nodes.len(),
                    "local scale: the sampled A grid left the safety band around the global scale; A stays the global scale"
                );
            }
        }
        let b: Vec<f32> = ref_nodes
            .iter()
            .zip(tgt_nodes.iter())
            .zip(a.iter())
            .map(|((&br, &bt), &ak)| br - ak * bt)
            .collect();

        grids.push(LnGrid {
            ref_width: reference.width,
            ref_height: reference.height,
            scale: scale_px,
            gw: expected_gw,
            gh: expected_gh,
            a,
            b,
            global_scale: channel_scale,
            location_ref: reference_for_detection.locations[p],
            location_tgt,
        });
    }

    // The one source's warping work for this frame is done — release its
    // displacement grid now rather than letting it ride to the end of the
    // function (the sidecar write below is pure I/O and touches no grid).
    drop(src);

    let t = Instant::now();
    LnFrameGrids { channels: grids }
        .write(sidecar)
        .map_err(|e| LnError::Other(format!("writing .athln sidecar: {e:#}")))?;
    let write_ms = t.elapsed().as_millis() as u64;

    let scale = scales.iter().sum::<f64>() / scales.len().max(1) as f64;

    Ok(LnFrameOutcome {
        sidecar: sidecar.to_path_buf(),
        scale,
        matches: matches_total,
        samples: samples_total,
        samples_rejected: samples_rejected_total,
        samples_shared: samples_shared_total,
        cells_unsupported: cells_unsupported_total,
        warp_ms,
        background_ms,
        scale_ms,
        write_ms,
        detect_ms,
        refine_ms,
        fit_ms,
        match_ms,
        ln_scale_source: frame_scale_source,
        channel_scales: scales,
        channel_from_seeds: from_seeds_per_channel,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stacking::psf_signal::PsfModel;

    fn ref_models(reference: &LnReference, scale: u32) -> Vec<BackgroundModel> {
        reference
            .planes
            .iter()
            .map(|p| {
                BackgroundModel::fit(
                    TileTable::measure(p, reference.width, reference.height, &CLEANING, None),
                    scale,
                )
                .expect("the fixture reference has a background")
            })
            .collect()
    }

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
            vec![
                1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0,
            ],
            vec![0.5f32; 12],
        ];
        let reference = reference_with_planes(planes);
        let for_detection =
            LnReferenceForDetection::build(&reference, PsfModel::default(), 50, None);
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
        let mut plane = vec![
            1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0,
        ];
        plane[5] = f32::NAN;
        let reference = reference_with_planes(vec![plane.clone()]);
        let location = median_of_finite(&plane);

        let for_detection =
            LnReferenceForDetection::build(&reference, PsfModel::default(), 50, None);
        assert_eq!(for_detection.sanitized_planes.len(), 1);
        match &for_detection.sanitized_planes[0] {
            Cow::Owned(v) => {
                assert!(
                    v.iter().all(|x| x.is_finite()),
                    "sanitized plane must be all-finite"
                );
                assert!(
                    (v[5] as f64 - location).abs() < 1e-9,
                    "the NaN must be replaced by the plane's own finite median: {} vs {location}",
                    v[5]
                );
            }
            Cow::Borrowed(_) => panic!("a plane with a NaN must not be borrowed as-is"),
        }
    }

    /// Perf tier A Task 12 pin: `median_of_finite` (now `median_in_place`
    /// on the owned `finite` buffer, no second copy) must still return
    /// exactly what a from-scratch sort-based median does, on a plane
    /// carrying NaNs, `±Inf`, and ties — the finite-filtering and the
    /// even/odd averaging are the parts a refactor could get wrong, not
    /// the allocation count. The naive reference is written in `f32`
    /// arithmetic for the even-count average (`0.5f32 * (lo + hi)`, cast
    /// to `f64` only at the end) to match `median_in_place`'s own
    /// arithmetic bit for bit — comparing against an `f64`-arithmetic
    /// average would fail on rounding alone, which is not what this pin is
    /// checking.
    #[test]
    fn median_of_finite_matches_a_naive_sort_based_median() {
        fn naive_median_of_finite(plane: &[f32]) -> f64 {
            let mut finite: Vec<f32> = plane.iter().copied().filter(|v| v.is_finite()).collect();
            if finite.is_empty() {
                return f64::NAN;
            }
            finite.sort_by(|a, b| a.total_cmp(b));
            let n = finite.len();
            let mid = n / 2;
            let result: f32 = if n % 2 == 1 {
                finite[mid]
            } else {
                0.5f32 * (finite[mid - 1] + finite[mid])
            };
            result as f64
        }

        let cases: Vec<Vec<f32>> = vec![
            vec![3.0, 1.0, 2.0],                       // odd count
            vec![1.0, 2.0, 2.0, 2.0, 3.0, 4.0],         // even count, ties at the median
            vec![
                1.0,
                f32::NAN,
                2.0,
                f32::INFINITY,
                3.0,
                f32::NEG_INFINITY,
                4.0,
            ], // NaN/+-Inf mixed in among finite values
            vec![5.0, 5.0, 5.0, 5.0],                   // all ties
            vec![f32::NAN, f32::INFINITY, f32::NEG_INFINITY], // nothing finite
            vec![],                                     // empty plane
        ];

        for plane in cases {
            let got = median_of_finite(&plane);
            let want = naive_median_of_finite(&plane);
            if want.is_nan() {
                assert!(got.is_nan(), "plane {plane:?}: expected NaN, got {got}");
            } else {
                assert_eq!(
                    got.to_bits(),
                    want.to_bits(),
                    "plane {plane:?}: {got} vs {want}"
                );
            }
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
        let (a, used) = a_grid(None, TEST_SCALE, 1.0, gw, gh, GRID_STRIDE, GRID_W, GRID_H);
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
            1.0,
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
            1.0,
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

    // ---- Perf tier 1 Task 8: one `RegisteredSource` per frame ----------

    /// Guards the restructure of [`normalize_frame`]'s warp step: reading a
    /// 3-channel (OSC-shaped) frame's planes by re-pointing ONE
    /// `RegisteredSource` with `set_plane` must warp each channel
    /// bit-for-bit identically to the old per-channel shape (a fresh
    /// `RegisteredSource::open` per channel). Everything `normalize_frame`
    /// does past the warp (the background model, `relative_scale_against`, the
    /// `.athln` grid it writes) is a pure function of the warped `target`
    /// buffer, so comparing `target` directly — rather than the sidecar
    /// bytes `normalize_frame` itself produces, which would need a second
    /// code path through the whole function to get an "old shape" oracle —
    /// is the whole pin. This passes unchanged before and after the
    /// restructure below: it guards the invariant the restructure relies
    /// on (that `set_plane` reads the same bytes a fresh open would), not a
    /// behaviour the restructure introduces.
    #[test]
    fn one_source_per_frame_warps_each_channel_identically() {
        use crate::fits_writer::write_fits_f32;
        use crate::geometry::{Linear, PixelMap};
        use crate::resample::Interpolation;
        use crate::test_support::gaussian_field;

        let w = 24;
        let h = 20;
        let stars = [(6.3, 5.7, 900.0), (17.0, 13.4, 600.0)];
        let mut data = vec![0f32; w * h * 3];
        for (c, plane) in data.chunks_exact_mut(w * h).enumerate() {
            plane.copy_from_slice(&gaussian_field(w, h, &stars, 1.6, 100.0 + c as f32 * 25.0));
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("osc.fits");
        write_fits_f32(&path, w, h, 3, &data, &[]).unwrap();
        let map = PixelMap::linear(Linear::identity()).unwrap();
        let cancel = AtomicBool::new(false);
        let no_progress = |_: u64| {};

        // Old shape: a fresh `RegisteredSource` per channel.
        let mut old_targets = Vec::new();
        for p in 0..3 {
            let src = RegisteredSource::open(
                &[RegisteredFrame {
                    path: path.clone(),
                    map: map.clone(),
                }],
                w,
                h,
                p,
                Interpolation::BicubicBSpline,
                0.3,
            )
            .unwrap();
            let mut band = BandPlanes::new(&src);
            src.read_band_with_progress(0, h, &mut band, 1, &no_progress, &cancel)
                .unwrap();
            let mut target = vec![0f32; w * h];
            band.decode_frame_into(0, &mut target);
            old_targets.push(target);
        }

        // New shape: one source, re-pointed at each channel with `set_plane`.
        let mut new_targets = Vec::new();
        let mut src = RegisteredSource::open(
            &[RegisteredFrame {
                path: path.clone(),
                map: map.clone(),
            }],
            w,
            h,
            0,
            Interpolation::BicubicBSpline,
            0.3,
        )
        .unwrap();
        let mut band = BandPlanes::new(&src);
        for p in 0..3 {
            src.set_plane(p).unwrap();
            src.read_band_with_progress(0, h, &mut band, 1, &no_progress, &cancel)
                .unwrap();
            let mut target = vec![0f32; w * h];
            band.decode_frame_into(0, &mut target);
            new_targets.push(target);
        }

        for p in 0..3 {
            assert_eq!(
                old_targets[p], new_targets[p],
                "channel {p}: warped target differs between the per-channel-reopen shape and set_plane"
            );
        }
    }

    // ---- Perf tier C Task 2 fix round 4 (ruling C-14) -------------------

    /// Ruling C-14 item 5 (the review's M6): a channel carrying a fitted
    /// local-scale spline is calibrated by scaling the SAMPLED surface, not
    /// by being skipped. Every node must come out at `k · (s + spline)` —
    /// the constant term AND the residual — and the safety band's verdict
    /// must not move because `k` exists (it is measured on the UNSCALED
    /// deviation, which for `k > 0` is the identical test).
    #[test]
    fn a_grid_scales_the_whole_sampled_surface_by_the_calibration_factor() {
        let (gw, gh) = LnGrid::grid_dims(GRID_W, GRID_H, GRID_STRIDE);
        let spline = linear_residual_spline(1.0);
        let k = 0.9933;

        let (plain, plain_used) = a_grid(
            Some(&spline),
            TEST_SCALE,
            1.0,
            gw,
            gh,
            GRID_STRIDE,
            GRID_W,
            GRID_H,
        );
        let (scaled, scaled_used) = a_grid(
            Some(&spline),
            TEST_SCALE,
            k,
            gw,
            gh,
            GRID_STRIDE,
            GRID_W,
            GRID_H,
        );
        assert!(
            plain_used && scaled_used,
            "both surfaces are inside the band"
        );

        for j in 0..gh {
            let y = (j * GRID_STRIDE).min(GRID_H - 1) as f64;
            for i in 0..gw {
                let x = (i * GRID_STRIDE).min(GRID_W - 1) as f64;
                let want = (k * (TEST_SCALE + spline.displacement(x, y).0)) as f32;
                assert_eq!(
                    scaled[j * gw + i],
                    want,
                    "node ({i}, {j}) must be k*(s + spline), not k*s + spline"
                );
                // And it is genuinely the whole surface that moved, not
                // just its constant term: the RESIDUAL scales too.
                let plain_residual = plain[j * gw + i] as f64 - TEST_SCALE;
                let scaled_residual = scaled[j * gw + i] as f64 - k * TEST_SCALE;
                if plain_residual.abs() > 1e-6 {
                    assert!(
                        (scaled_residual - k * plain_residual).abs() < 1e-6,
                        "node ({i}, {j}): residual {scaled_residual} is not k*{plain_residual}"
                    );
                }
            }
        }

        // A surface the band refuses is refused whatever `k` is — and the
        // fallback constant is the CALIBRATED one.
        let wild = linear_residual_spline(30.0);
        let (fallback, used) = a_grid(
            Some(&wild),
            TEST_SCALE,
            k,
            gw,
            gh,
            GRID_STRIDE,
            GRID_W,
            GRID_H,
        );
        assert!(!used, "the band verdict must not depend on k");
        assert!(
            fallback.iter().all(|&v| v == (TEST_SCALE * k) as f32),
            "the refused-surface fallback is the CALIBRATED constant"
        );
    }

    /// The acceptance report attributes every "ln relative scale" event to a
    /// frame and plane by its span (`ln_plane{frame_id, plane}`), and tells
    /// the seeds calibration's throwaway arms apart by `ln_seeds_calibration`.
    #[test]
    fn the_scale_event_carries_its_frame_and_plane_and_the_calibration_arm_is_tagged() {
        use crate::fits_writer::write_fits_f32;
        use crate::geometry::{Linear, PixelMap};
        use crate::resample::Interpolation;
        use crate::stacking::measure::FrameMeasurement;
        use crate::stacking::test_fixtures::synthetic_star_field;
        use crate::stacking::weights::FrameWeight;

        const W: usize = 512;
        const H: usize = 384;
        let mut stars: Vec<(f64, f64, f64)> = Vec::new();
        for row in 0..6 {
            for col in 0..10 {
                let x = 26.0 + col as f64 * 46.0 + ((row * 7 + col) % 5) as f64;
                let y = 24.0 + row as f64 * 58.0 + ((row * 3 + col) % 4) as f64;
                stars.push((x, y, 0.10 + ((row * 10 + col) % 9) as f64 * 0.02));
            }
        }
        let reference_plane = synthetic_star_field(W, H, &stars, 4.2, 0.002, 11);
        let target_stars: Vec<(f64, f64, f64)> =
            stars.iter().map(|&(x, y, a)| (x, y, a * 0.9)).collect();
        let target_plane = synthetic_star_field(W, H, &target_stars, 4.2, 0.002, 12);
        let dir = tempfile::tempdir().unwrap();
        let target_path = dir.path().join("target.fits");
        write_fits_f32(&target_path, W, H, 1, &target_plane, &[]).unwrap();

        let reference = LnReference {
            width: W,
            height: H,
            planes: vec![reference_plane],
            frames_used: Vec::new(),
        };
        let cfg = LocalNormalizationConfig {
            enabled: true,
            scale: 256,
            ..LocalNormalizationConfig::default()
        };
        let measure = crate::stacking::measure::MeasureOptions {
            psf_model: PsfModel::Moffat4,
            max_stars: 200,
            ..Default::default()
        };
        let for_detection =
            LnReferenceForDetection::build(&reference, PsfModel::Moffat4, measure.max_stars, None);
        let ref_bg = ref_models(&reference, cfg.scale);
        let frame = StackFrame {
            path: target_path,
            map: PixelMap::linear(Linear::identity()).unwrap(),
            measurement: FrameMeasurement {
                width: W,
                height: H,
                channels: vec![Default::default()],
                duration_ms: 0,
            },
            weight: FrameWeight {
                channels: vec![1.0],
                normalized: vec![1.0],
                mean: 1.0,
                normalized_mean: 1.0,
                missing: None,
            },
            exposure_s: 180.0,
            date_obs: None,
            frame_id: 7,
            registered_path: None,
        };
        let cancel = AtomicBool::new(false);
        let scale_lines = |logs: &str| -> Vec<String> {
            logs.lines()
                .filter(|l| l.contains("ln relative scale"))
                .map(str::to_string)
                .collect()
        };

        let sidecar = dir.path().join("main.athln");
        let logs = crate::stacking::solve::tests::captured_logs(|| {
            normalize_frame(
                &reference,
                &for_detection,
                &ref_bg,
                &frame,
                LnScaleSeeds::Measured(&[Vec::new()]),
                &cfg,
                cfg.scale,
                &measure,
                Interpolation::BicubicBSpline,
                0.3,
                &sidecar,
                None,
                None,
                &cancel,
            )
            .expect("a clean synthetic field must normalize");
        });
        let lines = scale_lines(&logs);
        assert!(!lines.is_empty(), "no scale event in:\n{logs}");
        for l in &lines {
            assert!(l.contains("ln_plane{frame_id=7 plane=0}"), "{l}");
            assert!(!l.contains("ln_seeds_calibration"), "{l}");
        }

        let logs = crate::stacking::solve::tests::captured_logs(|| {
            let _ = measure_seeds_calibration(
                &reference,
                &for_detection,
                &ref_bg,
                std::slice::from_ref(&frame),
                &[0],
                &|_| vec![Vec::new()],
                &cfg,
                cfg.scale,
                &measure,
                Interpolation::BicubicBSpline,
                0.3,
                &dir.path().join("cal"),
                None,
                &cancel,
                &|_, _, _| {},
            );
        });
        let lines = scale_lines(&logs);
        assert!(
            !lines.is_empty(),
            "no scale event in the calibration arm:\n{logs}"
        );
        for l in &lines {
            assert!(
                l.contains("ln_seeds_calibration") && l.contains("ln_plane{frame_id=7 plane=0}"),
                "{l}"
            );
        }
    }

    /// Ruling C-14 item 7: end to end through `normalize_frame`, the
    /// written `.athln` must carry `A = k·s` and `B = B_ref − A·B_tgt` —
    /// read back with the real [`LnFrameGrids::read`], not by inspecting
    /// the in-memory grids. The pin compares an uncalibrated run against a
    /// calibrated one over the SAME frame and reference, so `s` and `B_tgt`
    /// are identical between them and `k` is the only thing that moved.
    #[test]
    fn the_written_sidecar_carries_the_calibrated_a_and_a_matching_b() {
        use crate::fits_writer::write_fits_f32;
        use crate::geometry::{Linear, PixelMap};
        use crate::resample::Interpolation;
        use crate::stacking::measure::FrameMeasurement;
        use crate::stacking::test_fixtures::synthetic_star_field;
        use crate::stacking::weights::FrameWeight;

        const W: usize = 512;
        const H: usize = 384;
        const K: f64 = 0.9933;

        // 10x6 stars on a jittered grid, the same field family
        // `ln::scale`'s own tests use — comfortably past `MIN_MATCHES`.
        let mut stars: Vec<(f64, f64, f64)> = Vec::new();
        for row in 0..6 {
            for col in 0..10 {
                let x = 26.0 + col as f64 * 46.0 + ((row * 7 + col) % 5) as f64;
                let y = 24.0 + row as f64 * 58.0 + ((row * 3 + col) % 4) as f64;
                let amp = 0.10 + ((row * 10 + col) % 9) as f64 * 0.02;
                stars.push((x, y, amp));
            }
        }
        let reference_plane = synthetic_star_field(W, H, &stars, 4.2, 0.002, 11);
        // The target is the same field a little fainter, so the measured
        // relative scale is a real number rather than exactly 1.
        let target_stars: Vec<(f64, f64, f64)> =
            stars.iter().map(|&(x, y, a)| (x, y, a * 0.9)).collect();
        let target_plane = synthetic_star_field(W, H, &target_stars, 4.2, 0.002, 12);

        let dir = tempfile::tempdir().unwrap();
        let target_path = dir.path().join("target.fits");
        write_fits_f32(&target_path, W, H, 1, &target_plane, &[]).unwrap();

        let reference = LnReference {
            width: W,
            height: H,
            planes: vec![reference_plane],
            frames_used: Vec::new(),
        };
        let cfg = LocalNormalizationConfig {
            enabled: true,
            scale: 256,
            ..LocalNormalizationConfig::default()
        };
        let measure = crate::stacking::measure::MeasureOptions {
            psf_model: PsfModel::Moffat4,
            max_stars: 200,
            ..Default::default()
        };
        let for_detection =
            LnReferenceForDetection::build(&reference, PsfModel::Moffat4, measure.max_stars, None);
        let ref_bg = ref_models(&reference, cfg.scale);

        let frame = StackFrame {
            path: target_path.clone(),
            map: PixelMap::linear(Linear::identity()).unwrap(),
            measurement: FrameMeasurement {
                width: W,
                height: H,
                channels: vec![Default::default()],
                duration_ms: 0,
            },
            weight: FrameWeight {
                channels: vec![1.0],
                normalized: vec![1.0],
                mean: 1.0,
                normalized_mean: 1.0,
                missing: None,
            },
            exposure_s: 180.0,
            date_obs: None,
            frame_id: 1,
            registered_path: None,
        };
        let cancel = AtomicBool::new(false);

        type Normalized = (LnFrameOutcome, LnFrameGrids);
        let run = |k: Option<&SeedsCalibration>, name: &str| -> Normalized {
            let sidecar = dir.path().join(format!("{name}.athln"));
            let fits: Vec<Vec<StarFit>> = vec![Vec::new()];
            let outcome = normalize_frame(
                &reference,
                &for_detection,
                &ref_bg,
                &frame,
                // No usable fits: the channel takes the DETECTION path,
                // which this pin deliberately exercises — `k` is applied
                // by `normalize_frame` to whatever the scale step produced
                // (the seeds/detection distinction is the CALLER's, tested
                // by the run pin; here the arithmetic is the subject).
                LnScaleSeeds::Measured(&fits),
                &cfg,
                cfg.scale,
                &measure,
                Interpolation::BicubicBSpline,
                0.3,
                &sidecar,
                k,
                None,
                &cancel,
            )
            .expect("a clean synthetic field must normalize");
            let grids = LnFrameGrids::read(&sidecar).expect("the sidecar reads back");
            (outcome, grids)
        };

        let calibration = SeedsCalibration {
            frame_ids: vec![frame.frame_id],
            k: vec![K],
        };
        let (plain, plain_grids) = run(None, "plain");
        // A channel that fell back to detection is NOT calibrated (the
        // fallback path has no measured bias to correct) — so this pin
        // first establishes that, then measures the arithmetic on the
        // seeds-path side by handing the scale through directly.
        assert_eq!(plain.ln_scale_source, "detected");
        assert_eq!(plain.channel_from_seeds, vec![false]);
        let (_calibrated, calibrated_grids) = run(Some(&calibration), "calibrated");
        assert_eq!(
            calibrated_grids.channels[0].a, plain_grids.channels[0].a,
            "a detection-path channel must be left uncalibrated"
        );

        // Now the seeds path, where `k` does apply: measure the target's
        // own fits and hand them in.
        let (_, target_fits) = crate::stacking::measure::measure_frame_with_fits(
            &target_path,
            &measure,
            None,
            &cancel,
        )
        .expect("measuring the target's own fits");
        let seeds_run = |k: Option<&SeedsCalibration>, name: &str| -> Normalized {
            let sidecar = dir.path().join(format!("{name}.athln"));
            let outcome = normalize_frame(
                &reference,
                &for_detection,
                &ref_bg,
                &frame,
                LnScaleSeeds::Measured(&target_fits),
                &cfg,
                cfg.scale,
                &measure,
                Interpolation::BicubicBSpline,
                0.3,
                &sidecar,
                k,
                None,
                &cancel,
            )
            .expect("a clean synthetic field must normalize");
            let grids = LnFrameGrids::read(&sidecar).expect("the sidecar reads back");
            (outcome, grids)
        };
        let (seeds_plain, seeds_plain_grids) = seeds_run(None, "seeds-plain");
        assert_eq!(seeds_plain.ln_scale_source, "seeds");
        assert_eq!(seeds_plain.channel_from_seeds, vec![true]);
        let (seeds_k, seeds_k_grids) = seeds_run(Some(&calibration), "seeds-calibrated");

        let s = seeds_plain.channel_scales[0];
        assert_eq!(
            seeds_k.channel_scales[0],
            s * K,
            "the reported channel scale is k*s"
        );
        let a_plain = &seeds_plain_grids.channels[0].a;
        let a_k = &seeds_k_grids.channels[0].a;
        assert!(
            a_k.iter().all(|&v| v == (s * K) as f32),
            "A = k*s at every node"
        );
        assert_eq!(
            seeds_k_grids.channels[0].global_scale,
            s * K,
            "the sidecar's own global scale carries k too"
        );

        // B = B_ref - A*B_tgt: the target background term is the SAME in
        // both runs (nothing about `k` touches the background model), so
        // the distance from B_ref scales by exactly the ratio of the two
        // `A`s.
        let b_plain = &seeds_plain_grids.channels[0].b;
        let b_k = &seeds_k_grids.channels[0].b;
        let ratio = (a_k[0] / a_plain[0]) as f64;
        assert!((ratio - K).abs() < 1e-6, "the A ratio is k: {ratio} vs {K}");
        let mut checked = 0usize;
        for (i, (&br, (&bp, &bk))) in ref_bg[0]
            .surface
            .nodes((cfg.scale / 8).max(2) as usize)
            .iter()
            .zip(b_plain.iter().zip(b_k.iter()))
            .enumerate()
        {
            let d_plain = (br - bp) as f64; // = A_plain * B_tgt
            let d_k = (br - bk) as f64; // = A_k     * B_tgt
            if d_plain.abs() < 1e-6 {
                continue;
            }
            assert!(
                (d_k - ratio * d_plain).abs() <= 1e-5 * d_plain.abs().max(1.0),
                "cell {i}: B_ref - B moved by {d_k}, expected {} (= k * {d_plain})",
                ratio * d_plain
            );
            checked += 1;
        }
        assert!(checked > 0, "the B pin must have checked real cells");
    }

    /// Plan 6b-3 D6: erosion by `margin` keeps a pixel only when every
    /// pixel within `margin` of it (inside the image) is valid; the image's
    /// own border is not an edge of the mask.
    #[test]
    fn eroded_mask_shrinks_the_valid_region_by_the_margin() {
        let (w, h) = (20usize, 10usize);
        let valid: Vec<bool> = (0..w * h).map(|k| k % w >= 5).collect();
        let e = eroded_mask(&valid, w, h, 3);
        for y in 0..h {
            for x in 0..w {
                assert_eq!(e[y * w + x], x >= 8, "({x}, {y})");
            }
        }
        assert!(eroded_mask(&vec![true; w * h], w, h, 3).iter().all(|&v| v));
    }

    /// Plan 6b-3 D6, spec §5.6: a member that covers only part of the LN
    /// reference — and a reference that covers only part of the member — is
    /// normalized on their overlap and recovers the truth `R = 1.25·T + 0.03`:
    /// the relative scale within 1 % and the additive term within 0.002 on
    /// every grid node inside the overlap eroded by the 48 px seed margin
    /// (review I2: what a node outside the overlap holds is
    /// the surface's continuation over masked tiles, which the method
    /// does not define). Under a quarter of overlap the frame is refused
    /// `LowOverlap` before any fit.
    #[test]
    fn masked_ln_recovers_a_half_covered_members_scale() {
        use crate::fits_writer::write_fits_f32;
        use crate::geometry::{Linear, PixelMap};
        use crate::resample::Interpolation;
        use crate::stacking::measure::FrameMeasurement;
        use crate::stacking::test_fixtures::synthetic_star_field;
        use crate::stacking::weights::FrameWeight;

        const W: usize = 512;
        const H: usize = 384;
        let mut stars: Vec<(f64, f64, f64)> = Vec::new();
        for row in 0..6 {
            for col in 0..10 {
                let x = 26.0 + col as f64 * 46.0 + ((row * 7 + col) % 5) as f64;
                let y = 24.0 + row as f64 * 58.0 + ((row * 3 + col) % 4) as f64;
                stars.push((x, y, 0.10 + ((row * 10 + col) % 9) as f64 * 0.02));
            }
        }
        let field = synthetic_star_field(W, H, &stars, 4.2, 0.002, 11);
        // The reference covers x < 420; the member covers x >= `from`.
        let reference_plane: Vec<f32> = field
            .iter()
            .enumerate()
            .map(|(k, &v)| if k % W < 420 { v } else { f32::NAN })
            .collect();
        let target_of = |from: usize| -> Vec<f32> {
            field
                .iter()
                .enumerate()
                .map(|(k, &v)| {
                    if k % W >= from {
                        (v - 0.03) / 1.25
                    } else {
                        f32::NAN
                    }
                })
                .collect()
        };
        let dir = tempfile::tempdir().unwrap();
        let reference = LnReference {
            width: W,
            height: H,
            planes: vec![reference_plane],
            frames_used: Vec::new(),
        };
        let cfg = LocalNormalizationConfig {
            enabled: true,
            scale: 256,
            // P-13: pre-selection pin (synthetic field has no saturated star); selection is pinned by the_selection_runs_on_the_detection_and_the_seeds_path.
            star_selection: crate::stacking::integrate::LnStarSelection::OFF,
            ..LocalNormalizationConfig::default()
        };
        let measure = crate::stacking::measure::MeasureOptions {
            psf_model: PsfModel::Moffat4,
            max_stars: 200,
            ..Default::default()
        };
        let for_detection = LnReferenceForDetection::build_masked(
            &reference,
            PsfModel::Moffat4,
            measure.max_stars,
            None,
        );
        assert_eq!(for_detection.overlap, LnOverlap::Masked);
        let ref_bg = ref_models(&reference, cfg.scale);
        let cancel = AtomicBool::new(false);
        let run = |from: usize, name: &str| {
            let path = dir.path().join(format!("{name}.fits"));
            write_fits_f32(&path, W, H, 1, &target_of(from), &[]).unwrap();
            let frame = StackFrame {
                path,
                map: PixelMap::linear(Linear::identity()).unwrap(),
                measurement: FrameMeasurement {
                    width: W,
                    height: H,
                    channels: vec![Default::default()],
                    duration_ms: 0,
                },
                weight: FrameWeight {
                    channels: vec![1.0],
                    normalized: vec![1.0],
                    mean: 1.0,
                    normalized_mean: 1.0,
                    missing: None,
                },
                exposure_s: 180.0,
                date_obs: None,
                frame_id: 1,
                registered_path: None,
            };
            let sidecar = dir.path().join(format!("{name}.athln"));
            normalize_frame(
                &reference,
                &for_detection,
                &ref_bg,
                &frame,
                LnScaleSeeds::ForcedDetection,
                &cfg,
                cfg.scale,
                &measure,
                Interpolation::Bilinear,
                0.3,
                &sidecar,
                None,
                None,
                &cancel,
            )
            .map(|outcome| (outcome, LnFrameGrids::read(&sidecar).unwrap()))
        };

        let (outcome, grids) = run(100, "half").expect("a 78 % overlap normalizes");
        assert!(
            (outcome.scale - 1.25).abs() < 0.0125,
            "scale {}",
            outcome.scale
        );
        // The overlap is x in 100..420; eroded by LN_MASK_MARGIN_PX (48) it is
        // x in 148..372, y in 0..384 (the image border counts as valid, so
        // only the x edges erode). Node (i, j) sits at pixel (i, j)·stride.
        let g = &grids.channels[0];
        let stride = g.stride();
        let mut checked = 0usize;
        for j in 0..g.gh {
            for i in 0..g.gw {
                let (x, y) = (i * stride, j * stride);
                if !(148..372).contains(&x) {
                    continue;
                }
                let b = g.b[j * g.gw + i];
                assert!((b - 0.03).abs() < 0.002, "B node at ({x}, {y}): {b}");
                checked += 1;
            }
        }
        assert!(checked >= 40, "{checked} nodes inside the eroded overlap");

        match run(400, "sliver") {
            Err(LnError::LowOverlap { fraction }) => {
                assert!(fraction < LN_MIN_OVERLAP, "{fraction}");
            }
            other => panic!("a 18 % overlap must be refused LowOverlap, got {other:?}"),
        }
    }

    /// Review of Task 4: a masked member's `(a, b)` coefficients are FINITE on
    /// every node of its registered footprint (and in fact everywhere),
    /// whatever part of it the LN reference does not cover. A non-finite
    /// coefficient would turn a raw-finite sample non-finite after
    /// normalization, over-counting coverage and leaving a pixel at 0.0
    /// instead of NaN under `empty_as_nan`.
    #[test]
    fn masked_ln_coefficients_are_finite_over_the_members_footprint() {
        use crate::fits_writer::write_fits_f32;
        use crate::geometry::{Linear, PixelMap};
        use crate::resample::Interpolation;
        use crate::stacking::measure::FrameMeasurement;
        use crate::stacking::test_fixtures::synthetic_star_field;
        use crate::stacking::weights::FrameWeight;

        const W: usize = 512;
        const H: usize = 384;
        let mut stars: Vec<(f64, f64, f64)> = Vec::new();
        for row in 0..14 {
            for col in 0..20 {
                let x = 14.0 + col as f64 * 25.0 + ((row * 7 + col) % 5) as f64;
                let y = 14.0 + row as f64 * 26.0 + ((row * 3 + col) % 4) as f64;
                stars.push((x, y, 0.10 + ((row * 10 + col) % 9) as f64 * 0.02));
            }
        }
        let field = synthetic_star_field(W, H, &stars, 4.2, 0.002, 11);
        // The reference covers x < 420 and y < 300; the member covers
        // x in 120..W and y >= 60, so each leaves a strip the other lacks.
        let reference_plane: Vec<f32> = field
            .iter()
            .enumerate()
            .map(|(k, &v)| {
                if k % W < 420 && k / W < 300 {
                    v
                } else {
                    f32::NAN
                }
            })
            .collect();
        let footprint = |k: usize| k % W >= 120 && k / W >= 60;
        let target: Vec<f32> = field
            .iter()
            .enumerate()
            .map(|(k, &v)| {
                if footprint(k) {
                    (v - 0.03) / 1.25
                } else {
                    f32::NAN
                }
            })
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let reference = LnReference {
            width: W,
            height: H,
            planes: vec![reference_plane],
            frames_used: Vec::new(),
        };
        let cfg = LocalNormalizationConfig {
            enabled: true,
            scale: 256,
            ..LocalNormalizationConfig::default()
        };
        let measure = crate::stacking::measure::MeasureOptions {
            psf_model: PsfModel::Moffat4,
            max_stars: 200,
            ..Default::default()
        };
        let for_detection = LnReferenceForDetection::build_masked(
            &reference,
            PsfModel::Moffat4,
            measure.max_stars,
            None,
        );
        let ref_bg = ref_models(&reference, cfg.scale);
        let path = dir.path().join("member.fits");
        write_fits_f32(&path, W, H, 1, &target, &[]).unwrap();
        let frame = StackFrame {
            path,
            map: PixelMap::linear(Linear::identity()).unwrap(),
            measurement: FrameMeasurement {
                width: W,
                height: H,
                channels: vec![Default::default()],
                duration_ms: 0,
            },
            weight: FrameWeight {
                channels: vec![1.0],
                normalized: vec![1.0],
                mean: 1.0,
                normalized_mean: 1.0,
                missing: None,
            },
            exposure_s: 180.0,
            date_obs: None,
            frame_id: 1,
            registered_path: None,
        };
        let sidecar = dir.path().join("member.athln");
        normalize_frame(
            &reference,
            &for_detection,
            &ref_bg,
            &frame,
            LnScaleSeeds::ForcedDetection,
            &cfg,
            cfg.scale,
            &measure,
            Interpolation::Bilinear,
            0.3,
            &sidecar,
            None,
            None,
            &AtomicBool::new(false),
        )
        .expect("a partially overlapping member normalizes");
        let grids = LnFrameGrids::read(&sidecar).unwrap();
        let g = &grids.channels[0];
        let stride = g.stride();
        let mut on_footprint = 0usize;
        for j in 0..g.gh {
            for i in 0..g.gw {
                let (a, b) = (g.a[j * g.gw + i], g.b[j * g.gw + i]);
                assert!(
                    a.is_finite() && b.is_finite(),
                    "node ({i}, {j}): a {a}, b {b}"
                );
                if footprint(((j * stride).min(H - 1)) * W + (i * stride).min(W - 1)) {
                    // Outside the overlap a node holds the flood-fill
                    // extension of both backgrounds, which is read: on this
                    // flat fixture it is the same truth B = 0.03.
                    assert!(
                        (b - 0.03).abs() < 0.002,
                        "footprint node ({}, {}): b {b}",
                        i * stride,
                        j * stride
                    );
                    on_footprint += 1;
                }
            }
        }
        assert!(on_footprint >= 20, "{on_footprint} nodes on the footprint");
    }

    /// One pixel invalid gives a (2m+1)^2 hole, clipped at the border: the
    /// column pass of [`eroded_mask`] spreads what the row pass found.
    #[test]
    fn eroded_mask_spreads_one_invalid_pixel_into_a_square_hole() {
        let (w, h, m) = (21usize, 15usize, 3usize);
        for (px, py) in [(10usize, 7usize), (1, 1), (20, 14)] {
            let mut valid = vec![true; w * h];
            valid[py * w + px] = false;
            let e = eroded_mask(&valid, w, h, m);
            for y in 0..h {
                for x in 0..w {
                    let in_hole = x.abs_diff(px) <= m && y.abs_diff(py) <= m;
                    assert_eq!(
                        e[y * w + x],
                        !in_hole,
                        "({x}, {y}) for hole at ({px}, {py})"
                    );
                }
            }
        }
    }

    /// The half-covered fixture shared by the bias guards and the seeds
    /// test: the reference covers x < `ref_cover`, the member x >= `from`,
    /// truth R = 1.25*T + 0.03. `seeds` hands the member's own measured
    /// fits in (the production path), else forced detection.
    fn masked_probe(
        interpolation: crate::resample::Interpolation,
        ref_cover: usize,
        from: usize,
        mode: LnOverlap,
        seeds: bool,
    ) -> (LnFrameOutcome, LnFrameGrids) {
        use crate::fits_writer::write_fits_f32;
        use crate::geometry::{Linear, PixelMap};
        use crate::stacking::measure::FrameMeasurement;
        use crate::stacking::test_fixtures::synthetic_star_field;
        use crate::stacking::weights::FrameWeight;

        const W: usize = 512;
        const H: usize = 384;
        let mut stars: Vec<(f64, f64, f64)> = Vec::new();
        for row in 0..6 {
            for col in 0..10 {
                let x = 26.0 + col as f64 * 46.0 + ((row * 7 + col) % 5) as f64;
                let y = 24.0 + row as f64 * 58.0 + ((row * 3 + col) % 4) as f64;
                stars.push((x, y, 0.10 + ((row * 10 + col) % 9) as f64 * 0.02));
            }
        }
        let field = synthetic_star_field(W, H, &stars, 4.2, 0.002, 11);
        let reference_plane: Vec<f32> = field
            .iter()
            .enumerate()
            .map(|(k, &v)| if k % W < ref_cover { v } else { f32::NAN })
            .collect();
        let target: Vec<f32> = field
            .iter()
            .enumerate()
            .map(|(k, &v)| {
                if k % W >= from {
                    (v - 0.03) / 1.25
                } else {
                    f32::NAN
                }
            })
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let reference = LnReference {
            width: W,
            height: H,
            planes: vec![reference_plane],
            frames_used: Vec::new(),
        };
        let cfg = LocalNormalizationConfig {
            enabled: true,
            scale: 256,
            // P-13: the probe pins coverage handling; its synthetic stars include no saturated one.
            star_selection: crate::stacking::integrate::LnStarSelection::OFF,
            ..LocalNormalizationConfig::default()
        };
        let measure = crate::stacking::measure::MeasureOptions {
            psf_model: PsfModel::Moffat4,
            max_stars: 200,
            ..Default::default()
        };
        let for_detection = match mode {
            LnOverlap::Masked => {
                LnReferenceForDetection::build_masked(&reference, PsfModel::Moffat4, 200, None)
            }
            LnOverlap::Legacy => {
                LnReferenceForDetection::build(&reference, PsfModel::Moffat4, 200, None)
            }
        };
        let ref_bg = ref_models(&reference, cfg.scale);
        let path = dir.path().join("member.fits");
        write_fits_f32(&path, W, H, 1, &target, &[]).unwrap();
        let cancel = AtomicBool::new(false);
        let fits = if seeds {
            Some(
                crate::stacking::measure::measure_frame_with_fits(&path, &measure, None, &cancel)
                    .expect("measuring the member's own fits")
                    .1,
            )
        } else {
            None
        };
        let frame = StackFrame {
            path,
            map: PixelMap::linear(Linear::identity()).unwrap(),
            measurement: FrameMeasurement {
                width: W,
                height: H,
                channels: vec![Default::default()],
                duration_ms: 0,
            },
            weight: FrameWeight {
                channels: vec![1.0],
                normalized: vec![1.0],
                mean: 1.0,
                normalized_mean: 1.0,
                missing: None,
            },
            exposure_s: 180.0,
            date_obs: None,
            frame_id: 1,
            registered_path: None,
        };
        let sidecar = dir.path().join("member.athln");
        let outcome = normalize_frame(
            &reference,
            &for_detection,
            &ref_bg,
            &frame,
            match &fits {
                Some(f) => LnScaleSeeds::Measured(f),
                None => LnScaleSeeds::ForcedDetection,
            },
            &cfg,
            cfg.scale,
            &measure,
            interpolation,
            0.3,
            &sidecar,
            None,
            None,
            &cancel,
        )
        .expect("the probe member normalizes");
        (outcome, LnFrameGrids::read(&sidecar).unwrap())
    }

    /// Bias guard (a): at full coverage Masked is a bit-exact no-op against
    /// Legacy, under the production warp kernel too.
    #[test]
    fn masked_ln_is_a_bit_exact_no_op_at_full_coverage() {
        use crate::resample::Interpolation;
        let bicubic = Interpolation::BicubicBSpline;
        let (lo, lg) = masked_probe(bicubic, 512, 0, LnOverlap::Legacy, false);
        let (mo, mg) = masked_probe(bicubic, 512, 0, LnOverlap::Masked, false);
        assert_eq!(lg, mg, "the sidecar grids must be identical");
        assert_eq!(lo.channel_scales, mo.channel_scales);
        assert_eq!(lo.matches, mo.matches);
    }

    /// Bias guard (b): under `BicubicBSpline` the half-covered Masked fit
    /// agrees with Legacy at full coverage to 0.3 %. Both sit ~2.7 % under
    /// 1.25 here: the fixture's reference is raw while the member is read
    /// through the smoothing B-spline kernel, a fixture asymmetry and not a
    /// property of masking.
    #[test]
    fn masked_ln_half_coverage_agrees_with_legacy_full_coverage_under_the_production_kernel() {
        use crate::resample::Interpolation;
        let bicubic = Interpolation::BicubicBSpline;
        let (full, _) = masked_probe(bicubic, 512, 0, LnOverlap::Legacy, false);
        let (half, _) = masked_probe(bicubic, 420, 100, LnOverlap::Masked, false);
        let rel = (half.scale - full.scale).abs() / full.scale;
        assert!(
            rel < 0.003,
            "half {} vs full {}: {rel}",
            half.scale,
            full.scale
        );
    }

    /// The production masked scale path: the member's own measured fits seed
    /// the fit (`relative_scale_from_seeds_masked`), same tolerances.
    #[test]
    fn masked_ln_recovers_the_half_covered_scale_from_measured_seeds() {
        use crate::resample::Interpolation;
        let (outcome, grids) =
            masked_probe(Interpolation::Bilinear, 420, 100, LnOverlap::Masked, true);
        assert_eq!(outcome.ln_scale_source, "seeds");
        assert_eq!(outcome.channel_from_seeds, vec![true]);
        assert!(
            (outcome.scale - 1.25).abs() < 0.0125,
            "scale {}",
            outcome.scale
        );
        let g = &grids.channels[0];
        let stride = g.stride();
        let mut checked = 0usize;
        for j in 0..g.gh {
            for i in 0..g.gw {
                let x = i * stride;
                if !(148..372).contains(&x) {
                    continue;
                }
                let b = g.b[j * g.gw + i];
                assert!((b - 0.03).abs() < 0.002, "B node at x {x}: {b}");
                checked += 1;
            }
        }
        assert!(checked >= 40, "{checked} nodes");
    }
}

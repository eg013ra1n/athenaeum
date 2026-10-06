//! The LN reference per group (spec §5.2, M2 Task 4): a low-noise,
//! resolution-shared integration of the group's best-weighted included
//! frames, built once and kept in RAM for the LN pass (M2 Task 5 measures
//! per-frame background/scale against it), and optionally written to
//! `ln/<group>/reference.fits` as an artifact ([`write_reference`] /
//! [`read_reference`]).
//!
//! [`build_reference`] shares [`crate::stacking::integrate::integrate_planes`]
//! — the ONE `RegisteredSource` per group (ruling R-T4-7: re-pointed at
//! each plane with `set_plane`) plus `integrate_stack` per plane that
//! `integrate_group` (Plan 4) also drives — rather than re-implementing it:
//! the only differences from a normal group integration are the frame
//! subset (the best `n` by weight, not every included frame), the weights
//! (all `1.0`, not the group's per-channel weights), the rejection recipe
//! (always linear-fit `5.0`/`3.5`, not the group's Auto-resolved choice) and
//! the normalization modes (always plain global `additiveWithScaling` /
//! `scaleZeroOffset`, never the group's own configured normalization or
//! `local`, which doesn't exist yet at reference-build time — the reference
//! is what later frames measure *against*).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Context;
use rayon::prelude::*;
use tracing::{debug, info};

use super::background::{TileTable, CLEANING};
use super::selection::SaturationMask;
use crate::fits_writer::{
    write_fits_f32, write_fits_f32_streaming_with, Card, CardValue, Durability, FitsWriteError,
};
use crate::integration::banded::BandPlanes;
use crate::integration::combine::{Combination, IntegrationRecipe, Rejection};
use crate::integration::engine::EngineProgress;
use crate::integration::io_policy::IoPolicy;
use crate::integration::plane_reader::PlaneReader;
use crate::integration::registered_source::RegisteredSource;
use crate::integration::source::FrameSource;
use crate::integration::stats::{OutputNormalization, RejectionNormalization};
use crate::integration::IntegrationError;
use crate::stacking::integrate::{
    integrate_planes, materialized_frame, validate_group_input, CoverageOptions, GroupInput,
    GroupProgress,
};
use crate::stacking::master_cards::ATH_STK_VERSION;

/// A group's LN reference: one `width × height` plane per channel, plus
/// which frames went into it. Lives in RAM until (optionally) written by
/// [`write_reference`].
#[derive(Debug)]
pub struct LnReference {
    pub width: usize,
    pub height: usize,
    /// One plane per channel, planar (never interleaved), `width × height`
    /// each — the same layout [`crate::fits_writer::write_fits_f32`] wants.
    pub planes: Vec<Vec<f32>>,
    /// Indices into the group's frame list that were combined, sorted by
    /// weight descending (the best-weighted frame first). Not persisted —
    /// [`write_reference`] stamps no card for it, so [`read_reference`]
    /// always comes back with this empty; a caller that needs the
    /// provenance keeps its own record of the selection (Task 9's run
    /// summary), it doesn't re-derive it from the file.
    pub frames_used: Vec<usize>,
}

/// Builds the group's LN reference (spec §5.2): the best `n` of `included`
/// by [`crate::stacking::weights::FrameWeight::normalized_mean`], integrated
/// through [`integrate_planes`] with equal (`1.0`) weights,
/// [`Rejection::LinearFitClip`] `{ sigma_low: 5.0, sigma_high: 3.5 }`,
/// output normalization [`OutputNormalization::AdditiveWithScaling`] and
/// rejection normalization [`RejectionNormalization::ScaleZeroOffset`] — the
/// group's own configured combination/rejection/normalization
/// (`input.integration`/`input.normalization`) are never consulted for any
/// of those four choices, only for `range_low`/`range_high`, which
/// `integrate_planes` always reads from `input.integration` regardless of
/// caller. Anchored on `input.reference` — the same frame `integrate_group`
/// itself normalizes the group against — whether or not that frame is one
/// of the `n` selected: the anchor is only ever read for its measurement,
/// never required to be part of the combined subset.
///
/// `included` is the group's frame list AFTER the weight-floor drop (spec
/// §6.2); fewer than 3 members (or fewer than 3 once `n` is applied — only
/// possible if the caller passes `n < 3`) is refused outright with
/// [`IntegrationError::BadInput`]. Ruling R3: a group with fewer than 3
/// included frames falls back to global normalization with a warning,
/// never a hard failure — but that fallback is the CALLER's job (Task 5),
/// not this function's; `build_reference` only ever reports the refusal.
///
/// Ruling R-M3-17 v2 (spec §4.4): `input.reference` — the anchor this
/// function's member integration normalizes to — is chosen SKY-PENALIZED
/// by the run (`stacking::weights::sky_penalized_order`), never just the
/// set's own registration reference. `included`'s own ranking is likewise
/// the run's business, not this function's: `build_reference` still ranks
/// whatever `included` it is handed by raw weight internally (unchanged)
/// and takes the best `n` of THAT — the caller (`stacking::run::
/// run_group_normalization`) achieves "best `n` by sky-penalized score"
/// not by changing this function, but by narrowing `included` to already
/// be exactly that top-`n` sky-penalized set before calling in, so the
/// internal by-weight re-sort only reorders those `n` (a no-op on which
/// members end up in `frames_used`).
/// [`validate_group_input`] runs first — the same reference-range and
/// per-frame channel-count checks `integrate_group` performs, shared so a
/// malformed `GroupInput` is refused here too instead of indexing out of
/// bounds inside `integrate_planes`.
///
/// `io` is the run's own [`IoPolicy`] — the same one `integrate_group`
/// takes, resolved once per run from the user's `integration.band_budget_mb`
/// setting and the frames' storage class (`run.rs`'s
/// `integration::io_policy::resolve` call, over the same calibrated files
/// this reference reads) — never minted locally, so a network-classified
/// group still gets its `Network` read-concurrency clamp and the user's
/// configured band budget actually applies to a reference build too.
///
/// `on_progress` receives `(plane_index, planes_total)` exactly like
/// [`GroupProgress::on_plane`] — band/combine-level progress inside the
/// engine is not surfaced (a no-op is passed through internally), since a
/// reference build is a small, second-order piece of a group's work.
///
/// `empty_as_nan` (plan 6b-3 D4/D5): a pixel no member frame covers comes
/// out NaN instead of 0.0 — set by a project's LN reference on its grid,
/// `false` for every frame-set caller.
#[allow(clippy::too_many_arguments)]
pub fn build_reference(
    input: &GroupInput<'_>,
    included: &[usize],
    n: usize,
    // An owned `Arc`, not a bare `&rayon::ThreadPool` (perf tier 1 Task 2):
    // forwarded straight into `integrate_planes`, which needs a clonable
    // handle for its own `RegisteredSource::with_pool`.
    pool: &Arc<rayon::ThreadPool>,
    cancel: &AtomicBool,
    io: IoPolicy,
    on_progress: &(dyn Fn(usize, usize) + Sync),
    // Plan 6b-3 D4/D5: a project's LN reference marks the pixels no member
    // covers NaN, so they are never data.
    empty_as_nan: bool,
) -> Result<LnReference, IntegrationError> {
    validate_group_input(input)?;
    if included.len() < 3 {
        return Err(IntegrationError::BadInput(format!(
            "{} included frames, need at least 3 for an LN reference",
            included.len()
        )));
    }

    // Best-weighted first: `sort_by` is stable, so frames tied on weight
    // keep `included`'s own relative order.
    let mut ranked: Vec<usize> = included.to_vec();
    ranked.sort_by(|&a, &b| {
        input.frames[b]
            .weight
            .normalized_mean
            .total_cmp(&input.frames[a].weight.normalized_mean)
    });
    let take = n.min(ranked.len());
    if take < 3 {
        return Err(IntegrationError::BadInput(format!(
            "referenceFrames={n} leaves only {take} of {} included frames, need at least 3",
            included.len()
        )));
    }
    let frames_used: Vec<usize> = ranked[..take].to_vec();

    info!(
        count = included.len(),
        ln_reference_frames = frames_used.len(),
        ln_reference_frames_configured = n,
        "LN reference: integrating the best-weighted included frames"
    );

    // Equal weighting (spec §5.2: "linear-fit rejection, global
    // normalization" — no weight scheme is named, and a reference meant to
    // be a plain, low-noise stand-in has no reason to favor one of its own
    // best-weighted members over another).
    let weights = reference_weights(input.channels, frames_used.len());
    let ReferenceRecipe {
        recipe,
        output_mode,
        rejection_mode,
        write_maps,
    } = reference_recipe();

    let on_band = |_: usize, _: usize, _: u64, _: u64| {};
    let progress = GroupProgress {
        on_plane: on_progress,
        engine: EngineProgress {
            on_band: &on_band,
            on_combine: &on_band,
        },
    };

    // M3 Task 2: the LN reference never writes rejection bitmaps — `rej:
    // None` — signature change only, this function's own behavior is
    // unaffected.
    let (outputs, _output_pairs) = integrate_planes(
        input,
        &frames_used,
        &weights,
        output_mode,
        rejection_mode,
        // M2: local normalization "doesn't exist yet at reference-build
        // time" (this function's own doc) — forced off independently of
        // whatever the group itself has configured, same reasoning as
        // `output_mode`/`rejection_mode` above.
        false,
        None,
        None,
        // M4c Task 3: and it never runs the large-scale second pass either
        // — `forced: None`, signature change only.
        None,
        recipe,
        write_maps,
        CoverageOptions {
            count: false,
            empty_as_nan,
        },
        pool,
        cancel,
        &progress,
        io,
    )?;

    let planes: Vec<Vec<f32>> = outputs.into_iter().map(|out| out.base.data).collect();

    Ok(LnReference {
        width: input.width,
        height: input.height,
        planes,
        frames_used,
    })
}

/// The fixed integration choices spec §5.2 mandates for a group's LN
/// reference, independent of the group's own configured combination/
/// rejection/normalization (`input.integration`/`input.normalization`):
/// linear-fit rejection `5.0`/`3.5`, `Average` combination, output
/// normalization `AdditiveWithScaling`, rejection normalization
/// `ScaleZeroOffset`, no rejection maps. Pulled into one function — and
/// pinned directly by a unit test — so a future refactor of
/// `build_reference` can never let the group's own config leak into what
/// must always be these same four choices.
struct ReferenceRecipe {
    recipe: IntegrationRecipe,
    output_mode: OutputNormalization,
    rejection_mode: RejectionNormalization,
    write_maps: bool,
}

fn reference_recipe() -> ReferenceRecipe {
    ReferenceRecipe {
        recipe: IntegrationRecipe {
            combination: Combination::Average,
            rejection: Rejection::LinearFitClip {
                sigma_low: 5.0,
                sigma_high: 3.5,
            },
        },
        output_mode: OutputNormalization::AdditiveWithScaling,
        rejection_mode: RejectionNormalization::ScaleZeroOffset,
        write_maps: false,
    }
}

/// Equal (`1.0`) per-channel weight for every one of `n` selected frames —
/// spec §5.2 names no weighting scheme for the reference, so every member
/// counts the same regardless of its own weight in the group.
fn reference_weights(channels: usize, n: usize) -> Vec<Vec<f32>> {
    vec![vec![1.0f32; channels]; n]
}

/// The cards every LN reference carries regardless of caller: `IMAGETYP`
/// (`'LN Reference'` — not one of `fits_writer::keywords::FrameKind`'s
/// values, so a plain card rather than `HeaderBuilder`, same as
/// `stacking::master_cards`'s own rejection-map cards) and the scanner-skip
/// pair `ATH_STK`/`ATH_STKV` — the identical cards (same keywords, same
/// version constant) a master light stamps, so the scanner's "a file
/// carrying `ATH_REG`/`ATH_STK` is never cataloged" rule (spec, scanner/mod.rs)
/// covers this artifact for free. `caller_cards` (e.g. `ATH_STKI`, the run
/// id; `ATH_STKG`, the group key — Task 5/9 own what they stamp) are
/// appended after these three.
fn reference_cards(caller_cards: &[Card]) -> Result<Vec<Card>, FitsWriteError> {
    stacking_cards("LN Reference", caller_cards)
}

/// [`reference_cards`] with another `IMAGETYP` — the saturation-map
/// sidecar (L-23) carries the same scanner-skip pair and caller cards.
fn stacking_cards(imagetyp: &str, caller_cards: &[Card]) -> Result<Vec<Card>, FitsWriteError> {
    let mut cards = vec![
        Card::new("IMAGETYP", CardValue::Str(imagetyp.into()))?,
        Card::new("ATH_STK", CardValue::Logical(true))?
            .with_comment("stacked by Athenaeum; never cataloged"),
        Card::new("ATH_STKV", CardValue::Integer(ATH_STK_VERSION))?
            .with_comment("stacking header version"),
    ];
    cards.extend(caller_cards.iter().cloned());
    Ok(cards)
}

/// Writes the reference as a float32 FITS (planar `channels × width ×
/// height`, atomic tmp+rename — the existing writer's own contract, so a
/// reader never observes a half-written file) at `path`, creating parent
/// directories as needed. `cards` is the caller's own provenance; the
/// mandatory `IMAGETYP`/scanner-skip cards ([`reference_cards`]) are added
/// here, not by the caller.
pub fn write_reference(r: &LnReference, path: &Path, cards: &[Card]) -> anyhow::Result<()> {
    let all_cards = reference_cards(cards)?;
    let mut data = Vec::with_capacity(r.width * r.height * r.planes.len());
    for plane in &r.planes {
        data.extend_from_slice(plane);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    write_fits_f32(path, r.width, r.height, r.planes.len(), &data, &all_cards)
        .with_context(|| format!("writing LN reference {}", path.display()))?;
    Ok(())
}

/// Reads a reference written by [`write_reference`] back into RAM.
/// `frames_used` always comes back empty — no card persists the selection
/// (see [`LnReference::frames_used`]'s own doc).
pub fn read_reference(path: &Path) -> anyhow::Result<LnReference> {
    let reader = PlaneReader::open(path)
        .with_context(|| format!("opening LN reference {}", path.display()))?;
    let (width, height, channels) = (reader.width(), reader.height(), reader.channels());
    let mut planes = Vec::with_capacity(channels);
    for p in 0..channels {
        planes.push(
            reader
                .read_plane(p)
                .with_context(|| format!("reading plane {p} of {}", path.display()))?,
        );
    }
    Ok(LnReference {
        width,
        height,
        planes,
        frames_used: Vec::new(),
    })
}

/// The sidecar's `IMAGETYP`.
const SATURATION_IMAGETYP: &str = "LN Reference Saturation";

/// One frame's contribution to the reference-frames saturation map (spec
/// §3 step 1, L-23): `map[x] = max(map[x], plane[x] / M)`, where `M` is the
/// frame's own hot-pixel-corrected plane maximum —
/// `TileTable::measure(plane, .., &CLEANING, ..).plane_max()`, the same
/// value the star selection compares a side with (`TileNoise::plane_max`).
/// A non-finite pixel contributes nothing; a frame whose `M` is not finite
/// or `<= 0` contributes nothing at all (the caller names it in a
/// `debug!`). The map starts all NaN, so a pixel no frame contributes to
/// stays NaN. Returns `M`.
pub fn accumulate_reference_saturation(
    map: &mut [f32],
    plane: &[f32],
    width: usize,
    height: usize,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> f32 {
    debug_assert_eq!(map.len(), width * height, "the map is not {width}x{height}");
    debug_assert_eq!(
        plane.len(),
        width * height,
        "the plane is not {width}x{height}"
    );
    let m = TileTable::measure(plane, width, height, &CLEANING, pool).plane_max();
    if !(m.is_finite() && m > 0.0) {
        return m;
    }
    let n = (width * height).min(map.len()).min(plane.len());
    let fold = move |(r, &p): (&mut f32, &f32)| {
        if p.is_finite() {
            let v = p / m;
            // `!(r >= v)`: a NaN `r` (no frame yet) takes `v` too.
            if !(*r >= v) {
                *r = v;
            }
        }
    };
    const CHUNK: usize = 1 << 16;
    match pool {
        Some(pl) => pl.install(|| {
            map[..n]
                .par_chunks_mut(CHUNK)
                .zip(plane[..n].par_chunks(CHUNK))
                .for_each(|(rc, pc)| rc.iter_mut().zip(pc).for_each(fold))
        }),
        None => map[..n].iter_mut().zip(&plane[..n]).for_each(fold),
    }
    m
}

/// The thresholded map the LN pass keeps in RAM (L-23): a bit where
/// `R > saturation`. NaN never sets a bit, and neither does a non-finite
/// threshold (`LnStarSelection::OFF`'s `+inf`).
pub fn reference_saturation_mask(
    map: &[f32],
    width: usize,
    height: usize,
    saturation: f64,
) -> SaturationMask {
    debug_assert_eq!(map.len(), width * height, "the map is not {width}x{height}");
    if !saturation.is_finite() {
        return SaturationMask::empty(width, height);
    }
    SaturationMask::from_fn(width, height, |k| {
        map.get(k).is_some_and(|&r| r as f64 > saturation)
    })
}

/// Builds the reference-frames saturation map over `frames_used` (the LN
/// reference's own members, indices into `input.frames`), one plane per
/// channel. Each frame is read through the SAME source
/// [`integrate_planes`] reads it through
/// ([`crate::stacking::integrate::materialized_frame`] +
/// [`RegisteredSource::open_materialized`], the group's interpolation and
/// clamping): its materialized registered artifact, or the on-the-fly warp
/// — so the map and the reference can never disagree about a frame's
/// pixels. One frame and one channel at a time: peak RAM is one frame
/// plane (plus the reader's band) on top of the maps. `cancel` is checked
/// before every frame and channel.
pub fn build_reference_saturation(
    input: &GroupInput<'_>,
    frames_used: &[usize],
    pool: &Arc<rayon::ThreadPool>,
    cancel: &AtomicBool,
) -> Result<Vec<Vec<f32>>, IntegrationError> {
    let (w, h) = (input.width, input.height);
    let mut maps = vec![vec![f32::NAN; w * h]; input.channels];
    let mut plane = vec![0f32; w * h];
    for &i in frames_used {
        if cancel.load(Ordering::Relaxed) {
            return Err(IntegrationError::Cancelled);
        }
        let frame = input.frames.get(i).ok_or_else(|| {
            IntegrationError::BadInput(format!(
                "frame index {i} of a {}-frame group",
                input.frames.len()
            ))
        })?;
        let mut src = RegisteredSource::open_materialized(
            &[materialized_frame(frame)],
            w,
            h,
            0,
            input.interpolation,
            input.clamping,
        )?
        .with_pool(Arc::clone(pool));
        let mut band = BandPlanes::new(&src);
        for (c, map) in maps.iter_mut().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Err(IntegrationError::Cancelled);
            }
            src.set_plane(c)?;
            src.read_band_with_progress(0, h, &mut band, 1, &|_: u64| {}, cancel)?;
            band.decode_frame_into(0, &mut plane);
            let m = accumulate_reference_saturation(map, &plane, w, h, Some(pool));
            if !(m.is_finite() && m > 0.0) {
                debug!(
                    frame_id = frame.frame_id,
                    plane = c,
                    "ln reference saturation map: the frame has no usable plane maximum"
                );
            }
        }
    }
    Ok(maps)
}

/// Writes the saturation map beside the reference (L-23):
/// `ln/<group>/reference-saturation.fits`, float32, the same planar layout
/// and caller cards (`ATH_STKI`/`ATH_STKG`) [`write_reference`]
/// gives `reference.fits`, plus the same scanner-skip pair.
///
/// Final review M-1: each plane's mask ([`reference_saturation_mask`] at
/// `saturation`) is built FIRST, then the plane is handed to the streaming
/// writer by `std::mem::take`, so no plane is ever cloned and the map's
/// planes are freed one by one as they are written (the caller's `maps`
/// planes are left empty). Returns the masks, one per plane — what the run
/// keeps. The file's bytes are those of the planes as given.
pub fn write_reference_saturation(
    maps: &mut [Vec<f32>],
    width: usize,
    height: usize,
    path: &Path,
    cards: &[Card],
    saturation: f64,
) -> anyhow::Result<Vec<SaturationMask>> {
    anyhow::ensure!(
        maps.iter().all(|m| m.len() == width * height),
        "LN reference saturation map: a plane is not {width}x{height}"
    );
    let all_cards = stacking_cards(SATURATION_IMAGETYP, cards)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let channels = maps.len();
    let mut masks = Vec::with_capacity(channels);
    // One plane at a time: never a second whole copy of the map.
    write_fits_f32_streaming_with(
        path,
        width,
        height,
        channels,
        &all_cards,
        Durability::Durable,
        |p| {
            masks.push(reference_saturation_mask(&maps[p], width, height, saturation));
            Ok(std::mem::take(&mut maps[p]))
        },
    )
    .with_context(|| format!("writing LN reference saturation map {}", path.display()))?;
    anyhow::ensure!(
        masks.len() == channels,
        "LN reference saturation map: {} of {channels} planes were written",
        masks.len()
    );
    Ok(masks)
}

/// Opens a saturation map, refusing another shape than the reference's.
fn open_reference_saturation(
    path: &Path,
    width: usize,
    height: usize,
    channels: usize,
) -> anyhow::Result<PlaneReader> {
    let reader = PlaneReader::open(path)
        .with_context(|| format!("opening LN reference saturation map {}", path.display()))?;
    let (w, h, c) = (reader.width(), reader.height(), reader.channels());
    if (w, h, c) != (width, height, channels) {
        anyhow::bail!(
            "LN reference saturation map {} is {w}x{h}x{c}, the reference is \
             {width}x{height}x{channels}",
            path.display()
        );
    }
    Ok(reader)
}

/// Reads a map written by [`write_reference_saturation`], refusing one
/// whose dimensions or channel count differ from the reference's.
pub fn read_reference_saturation(
    path: &Path,
    width: usize,
    height: usize,
    channels: usize,
) -> anyhow::Result<Vec<Vec<f32>>> {
    let reader = open_reference_saturation(path, width, height, channels)?;
    (0..channels)
        .map(|p| {
            reader
                .read_plane(p)
                .with_context(|| format!("reading plane {p} of {}", path.display()))
        })
        .collect()
}

/// [`read_reference_saturation`] thresholded plane by plane
/// ([`reference_saturation_mask`]), so the float map is never whole in RAM:
/// what the run keeps for a cached reference.
pub fn read_reference_saturation_masks(
    path: &Path,
    width: usize,
    height: usize,
    channels: usize,
    saturation: f64,
) -> anyhow::Result<Vec<SaturationMask>> {
    let reader = open_reference_saturation(path, width, height, channels)?;
    (0..channels)
        .map(|p| {
            let map = reader
                .read_plane(p)
                .with_context(|| format!("reading plane {p} of {}", path.display()))?;
            Ok(reference_saturation_mask(&map, width, height, saturation))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_parser::FitsHeader;
    use crate::fits_writer::write_fits_f32;
    use crate::geometry::{Linear, PixelMap};
    use crate::integration::storage_class::StorageClass;
    use crate::resample::Interpolation;
    use crate::stacking::integrate::{IntegrationConfig, NormalizationConfig, StackFrame};
    use crate::stacking::measure::{
        measure_frame, ChannelMeasurement, FrameMeasurement, MeasureOptions,
    };
    use crate::stacking::weights::FrameWeight;
    use crate::test_support::add_noise;
    use std::sync::Mutex;

    fn pool() -> Arc<rayon::ThreadPool> {
        Arc::new(
            rayon::ThreadPoolBuilder::new()
                .num_threads(2)
                .build()
                .unwrap(),
        )
    }

    /// Same shape as `stacking::integrate::tests`'s own `io()` helper — a
    /// generous local-storage policy that never becomes the bottleneck in a
    /// small synthetic fixture.
    fn io(band_budget_bytes: usize) -> IoPolicy {
        IoPolicy {
            band_budget_bytes,
            read_concurrency: 2,
            storage: StorageClass::Local,
        }
    }

    fn identity_map() -> PixelMap {
        PixelMap::linear(Linear::identity()).unwrap()
    }

    fn weight(w: f64) -> FrameWeight {
        FrameWeight {
            channels: vec![w],
            normalized: vec![w],
            mean: w,
            normalized_mean: w,
            missing: None,
        }
    }

    fn std_dev(data: &[f32]) -> f64 {
        let n = data.len() as f64;
        let mean = data.iter().map(|&v| v as f64).sum::<f64>() / n;
        let var = data.iter().map(|&v| (v as f64 - mean).powi(2)).sum::<f64>() / n;
        var.sqrt()
    }

    /// Six flat, star-free 96x64 frames (mirrors the min-weight-drop fixture
    /// in `stacking::integrate::tests` — a plain noisy plane is enough here
    /// too: this module only cares about weight-based selection and noise
    /// reduction, not registration or star measurement). Weights are
    /// deliberately NOT in frame-index order, so a test that expects
    /// `frames_used` sorted by weight actually exercises the sort rather
    /// than coincidentally matching `included`'s own order.
    fn six_frame_group(dir: &std::path::Path) -> (Vec<StackFrame>, Vec<Vec<f32>>) {
        const W: usize = 96;
        const H: usize = 64;
        const WEIGHTS: [f64; 6] = [0.5, 0.9, 0.3, 1.0, 0.1, 0.8];
        let mut raw = Vec::new();
        let mut frames = Vec::new();
        for (i, &w) in WEIGHTS.iter().enumerate() {
            let mut d = vec![0.1f32; W * H];
            add_noise(&mut d, 0.003, 1000 + i as u64);
            let path = dir.join(format!("f{i}.fits"));
            write_fits_f32(&path, W, H, 1, &d, &[]).unwrap();
            let measurement = measure_frame(
                &path,
                &MeasureOptions::default(),
                None,
                &AtomicBool::new(false),
            )
            .unwrap();
            raw.push(d);
            frames.push(StackFrame {
                path,
                map: identity_map(),
                measurement,
                weight: weight(w),
                exposure_s: 60.0,
                date_obs: None,
                frame_id: 900100 + i as i64,
                registered_path: None,
            });
        }
        (frames, raw)
    }

    #[test]
    fn build_reference_picks_the_best_weighted_frames_and_reduces_noise() {
        const W: usize = 96;
        const H: usize = 64;
        let dir = tempfile::tempdir().unwrap();
        let (frames, raw) = six_frame_group(dir.path());
        let included: Vec<usize> = (0..6).collect();

        let integration = IntegrationConfig::default();
        let normalization = NormalizationConfig::default();
        let input = GroupInput {
            frames: &frames,
            reference: 0,
            width: W,
            height: H,
            channels: 1,
            interpolation: Interpolation::Bilinear,
            clamping: 0.3,
            integration: &integration,
            normalization: &normalization,
            ln: None,
            rej: None,
            second_pass_bitmaps: false,
            coverage_map: false,
            stats_on_covered: false,
        };
        let pool = pool();
        let cancel = AtomicBool::new(false);
        let seen = Mutex::new(Vec::new());
        let on_progress = |p: usize, total: usize| seen.lock().unwrap().push((p, total));

        let r = build_reference(
            &input,
            &included,
            3,
            &pool,
            &cancel,
            io(20_000_000),
            &on_progress,
            false,
        )
        .unwrap();

        // Weights: idx3=1.0, idx1=0.9, idx5=0.8, idx0=0.5, idx2=0.3, idx4=0.1
        // — the top three, sorted descending, are [3, 1, 5].
        assert_eq!(r.frames_used, vec![3, 1, 5]);
        assert_eq!((r.width, r.height, r.planes.len()), (W, H, 1));
        assert_eq!(*seen.lock().unwrap(), vec![(0, 1)]);

        let ref_std = std_dev(&r.planes[0]);
        for (i, frame) in raw.iter().enumerate() {
            let s = std_dev(frame);
            assert!(
                ref_std < s,
                "frame {i}: reference std {ref_std} not lower than single-frame std {s}"
            );
        }
    }

    #[test]
    fn fewer_than_three_included_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (frames, _raw) = six_frame_group(dir.path());
        let integration = IntegrationConfig::default();
        let normalization = NormalizationConfig::default();
        let input = GroupInput {
            frames: &frames,
            reference: 0,
            width: 96,
            height: 64,
            channels: 1,
            interpolation: Interpolation::Bilinear,
            clamping: 0.3,
            integration: &integration,
            normalization: &normalization,
            ln: None,
            rej: None,
            second_pass_bitmaps: false,
            coverage_map: false,
            stats_on_covered: false,
        };
        let pool = pool();
        let cancel = AtomicBool::new(false);
        let on_progress = |_: usize, _: usize| {};
        let err = build_reference(
            &input,
            &[0, 1],
            3,
            &pool,
            &cancel,
            io(20_000_000),
            &on_progress,
            false,
        )
        .unwrap_err();
        match err {
            IntegrationError::BadInput(msg) => assert!(msg.contains("at least 3"), "{msg}"),
            other => panic!("expected BadInput, got {other:?}"),
        }
    }

    #[test]
    fn a_frame_with_the_wrong_channel_count_is_refused_not_panicked() {
        let dir = tempfile::tempdir().unwrap();
        let (mut frames, _raw) = six_frame_group(dir.path());
        // Corrupt one frame's own measurement to claim 2 channels while the
        // group (and every other frame) says 1 — `integrate_planes` would
        // index `measurement.channels[p]` out of bounds on this frame for
        // any `p >= 1` if `validate_group_input` didn't catch it first.
        frames[2].measurement = FrameMeasurement {
            width: 96,
            height: 64,
            channels: vec![ChannelMeasurement::default(); 2],
            duration_ms: 0,
        };
        let included: Vec<usize> = (0..6).collect();
        let integration = IntegrationConfig::default();
        let normalization = NormalizationConfig::default();
        let input = GroupInput {
            frames: &frames,
            reference: 0,
            width: 96,
            height: 64,
            channels: 1,
            interpolation: Interpolation::Bilinear,
            clamping: 0.3,
            integration: &integration,
            normalization: &normalization,
            ln: None,
            rej: None,
            second_pass_bitmaps: false,
            coverage_map: false,
            stats_on_covered: false,
        };
        let pool = pool();
        let cancel = AtomicBool::new(false);
        let on_progress = |_: usize, _: usize| {};
        let err = build_reference(
            &input,
            &included,
            3,
            &pool,
            &cancel,
            io(20_000_000),
            &on_progress,
            false,
        )
        .unwrap_err();
        match err {
            IntegrationError::BadInput(msg) => {
                assert!(msg.contains("frame 2"), "{msg}");
                assert!(msg.contains("measured channels"), "{msg}");
            }
            other => panic!("expected BadInput, got {other:?}"),
        }
    }

    #[test]
    fn reference_recipe_is_pinned_to_the_fixed_choices() {
        let r = reference_recipe();
        assert_eq!(r.recipe.combination, Combination::Average);
        assert_eq!(
            r.recipe.rejection,
            Rejection::LinearFitClip {
                sigma_low: 5.0,
                sigma_high: 3.5,
            }
        );
        assert_eq!(r.output_mode, OutputNormalization::AdditiveWithScaling);
        assert_eq!(r.rejection_mode, RejectionNormalization::ScaleZeroOffset);
        assert!(
            !r.write_maps,
            "the LN reference never writes rejection maps"
        );

        // Equal weighting: every one of `n` rows is all-`1.0`, `channels` wide.
        let w = reference_weights(3, 4);
        assert_eq!(w.len(), 4);
        for row in &w {
            assert_eq!(row, &vec![1.0f32; 3]);
        }
    }

    #[test]
    fn write_then_read_round_trips_planes_and_geometry() {
        let dir = tempfile::tempdir().unwrap();
        let r = LnReference {
            width: 12,
            height: 8,
            planes: vec![
                (0..12 * 8).map(|i| i as f32 * 0.01).collect(),
                (0..12 * 8).map(|i| 1.0 - i as f32 * 0.01).collect(),
                (0..12 * 8).map(|i| (i as f32).sin()).collect(),
            ],
            frames_used: vec![4, 2, 0],
        };
        let path = dir.path().join("reference.fits");
        write_reference(&r, &path, &[]).unwrap();

        let back = read_reference(&path).unwrap();
        assert_eq!((back.width, back.height), (r.width, r.height));
        assert_eq!(back.planes.len(), r.planes.len());
        for (a, b) in r.planes.iter().zip(back.planes.iter()) {
            assert_eq!(a, b);
        }
        // Not persisted (see the doc on `LnReference::frames_used`).
        assert!(back.frames_used.is_empty());
    }

    #[test]
    fn written_cards_carry_the_run_id_and_the_scanner_skip_card() {
        let dir = tempfile::tempdir().unwrap();
        let r = LnReference {
            width: 4,
            height: 4,
            planes: vec![vec![0.1f32; 16]],
            frames_used: vec![0, 1, 2],
        };
        let path = dir.path().join("reference.fits");
        let cards = vec![Card::new("ATH_STKI", CardValue::Str("run-42".into())).unwrap()];
        write_reference(&r, &path, &cards).unwrap();

        let header = FitsHeader::from_path(&path).unwrap();
        assert_eq!(header.get_str("IMAGETYP").as_deref(), Some("LN Reference"));
        // The scanner-skip card, same convention as a master light.
        assert_eq!(header.get_str("ATH_STK").as_deref(), Some("T"));
        assert_eq!(header.get_i32("ATH_STKV"), Some(ATH_STK_VERSION as i32));
        assert_eq!(header.get_str("ATH_STKI").as_deref(), Some("run-42"));
    }

    /// Plan 6b-3 D5 (review of Task 4): with `empty_as_nan`, each plane of a
    /// multi-channel reference is NaN at exactly the pixels no member covers
    /// IN THAT PLANE, and finite on every covered one; with it off the same
    /// pixels are 0.0 (the frame-set behaviour).
    #[test]
    fn empty_as_nan_marks_each_planes_own_uncovered_pixels() {
        const W: usize = 96;
        const H: usize = 64;
        const C: usize = 3;
        // Plane p is covered on x < COVER[p] by every member.
        const COVER: [usize; C] = [40, 60, 80];
        let dir = tempfile::tempdir().unwrap();
        let mut frames = Vec::new();
        for i in 0..4usize {
            let mut data = Vec::with_capacity(W * H * C);
            for (p, &cover) in COVER.iter().enumerate() {
                let mut plane = vec![0.1f32 + 0.05 * p as f32; W * H];
                add_noise(&mut plane, 0.003, 3000 + (i * C + p) as u64);
                for (k, v) in plane.iter_mut().enumerate() {
                    if k % W >= cover {
                        *v = f32::NAN;
                    }
                }
                data.extend_from_slice(&plane);
            }
            let path = dir.path().join(format!("rgb{i}.fits"));
            write_fits_f32(&path, W, H, C, &data, &[]).unwrap();
            frames.push(StackFrame {
                path,
                map: identity_map(),
                measurement: FrameMeasurement {
                    width: W,
                    height: H,
                    channels: vec![ChannelMeasurement::default(); C],
                    duration_ms: 0,
                },
                weight: FrameWeight {
                    channels: vec![1.0; C],
                    normalized: vec![1.0; C],
                    mean: 1.0,
                    normalized_mean: 1.0,
                    missing: None,
                },
                exposure_s: 60.0,
                date_obs: None,
                frame_id: 900200 + i as i64,
                registered_path: None,
            });
        }
        let integration = IntegrationConfig::default();
        let normalization = NormalizationConfig::default();
        let input = GroupInput {
            frames: &frames,
            reference: 0,
            width: W,
            height: H,
            channels: C,
            interpolation: Interpolation::Bilinear,
            clamping: 0.3,
            integration: &integration,
            normalization: &normalization,
            ln: None,
            rej: None,
            second_pass_bitmaps: false,
            coverage_map: false,
            stats_on_covered: false,
        };
        let pool = pool();
        let cancel = AtomicBool::new(false);
        let included: Vec<usize> = (0..4).collect();
        for empty_as_nan in [true, false] {
            let r = build_reference(
                &input,
                &included,
                3,
                &pool,
                &cancel,
                io(20_000_000),
                &|_, _| {},
                empty_as_nan,
            )
            .unwrap();
            assert_eq!(r.planes.len(), C);
            for (p, plane) in r.planes.iter().enumerate() {
                for (k, &v) in plane.iter().enumerate() {
                    // The footprint's last column is the warp's own edge: the
                    // bilinear kernel there reaches a NaN neighbour, so every
                    // member is NaN at it. Deterministic, hence pinned like
                    // the uncovered pixels (NaN when true, 0.0 when false).
                    let covered = k % W + 1 < COVER[p];
                    if covered {
                        assert!(v.is_finite(), "plane {p} pixel {k} is covered, got {v}");
                    } else if empty_as_nan {
                        assert!(v.is_nan(), "plane {p} pixel {k} is uncovered, got {v}");
                    } else {
                        assert_eq!(v, 0.0, "plane {p} pixel {k}");
                    }
                }
            }
        }
    }

    // ---- L-23: the reference-frames saturation map ----------------------

    const SW: usize = 96;
    const SH: usize = 64;

    /// A noisy 0.1 plane with a 5 × 5 core of `v` centred on `(cx, cy)`.
    fn plane_with_core(seed: u64, cx: usize, cy: usize, v: f32) -> Vec<f32> {
        let mut p = vec![0.1f32; SW * SH];
        add_noise(&mut p, 0.002, seed);
        for y in cy - 2..=cy + 2 {
            for x in cx - 2..=cx + 2 {
                p[y * SW + x] = v;
            }
        }
        p
    }

    fn bits(mask: &SaturationMask) -> Vec<bool> {
        (0..SW * SH)
            .map(|k| mask.get((k % SW) as isize, (k / SW) as isize))
            .collect()
    }

    /// T2: each frame's ratio is taken to its OWN hot-pixel-corrected
    /// maximum (a lone hot pixel does not set it), NaN pixels contribute
    /// nothing, `R` is NaN where every frame is NaN, a frame with no usable
    /// maximum contributes nothing, and at chosen pixels `R` is exactly the
    /// max over frames of `p / M`.
    #[test]
    fn the_saturation_map_is_the_max_of_each_frames_own_relative_level() {
        // Frame 0: a 0.8 core and a lone 1.0 hot pixel; NaN on x >= 80.
        let mut f0 = plane_with_core(11, 20, 20, 0.8);
        f0[30 * SW + 40] = 1.0;
        // Frame 1: a 0.4 core on the same star, a 0.9 core elsewhere; NaN
        // on x >= 64.
        let mut f1 = plane_with_core(12, 20, 20, 0.4);
        for y in 38..=42 {
            for x in 8..=12 {
                f1[y * SW + x] = 0.9;
            }
        }
        for k in 0..SW * SH {
            if k % SW >= 80 {
                f0[k] = f32::NAN;
            }
            if k % SW >= 64 {
                f1[k] = f32::NAN;
            }
        }
        let mut map = vec![f32::NAN; SW * SH];
        let m0 = accumulate_reference_saturation(&mut map, &f0, SW, SH, None);
        let m1 = accumulate_reference_saturation(&mut map, &f1, SW, SH, Some(&pool()));
        assert_eq!(m0, 0.8, "the hot pixel does not set frame 0's maximum");
        assert_eq!(m1, 0.9);

        // A frame with no usable maximum contributes nothing.
        let before: Vec<u32> = map.iter().map(|v| v.to_bits()).collect();
        let zeros = vec![0.0f32; SW * SH];
        assert_eq!(
            accumulate_reference_saturation(&mut map, &zeros, SW, SH, None),
            0.0
        );
        let all_nan = vec![f32::NAN; SW * SH];
        assert!(accumulate_reference_saturation(&mut map, &all_nan, SW, SH, None).is_nan());
        let after: Vec<u32> = map.iter().map(|v| v.to_bits()).collect();
        assert_eq!(before, after, "an unusable frame leaves the map alone");

        let at = |x: usize, y: usize| map[y * SW + x];
        let ratio = |p: &[f32], m: f32, x: usize, y: usize| p[y * SW + x] / m;
        // The star: frame 0 at its own maximum.
        assert_eq!(at(20, 20), 1.0);
        // The hot pixel itself still reads its raw value over M (1.0 / 0.8).
        assert_eq!(
            at(40, 30),
            ratio(&f0, m0, 40, 30).max(ratio(&f1, m1, 40, 30))
        );
        assert_eq!(at(40, 30), 1.25);
        // Frame 1's own star: 0.9 / 0.9.
        assert_eq!(at(10, 40), 1.0);
        // A background pixel: the larger of the two ratios, exactly.
        for (x, y) in [(5, 5), (50, 50), (60, 3)] {
            assert_eq!(at(x, y), ratio(&f0, m0, x, y).max(ratio(&f1, m1, x, y)));
        }
        // Only frame 0 covers 64 <= x < 80.
        assert_eq!(at(70, 10), ratio(&f0, m0, 70, 10));
        // No frame covers x >= 80.
        for y in 0..SH {
            for x in 80..SW {
                assert!(at(x, y).is_nan(), "({x}, {y}) = {}", at(x, y));
            }
        }

        // The mask: R > saturation, NaN never.
        let mask = reference_saturation_mask(&map, SW, SH, 0.75);
        assert!(mask.get(20, 20) && mask.get(40, 30) && mask.get(10, 40));
        assert!(!mask.get(5, 5) && !mask.get(85, 10));
        let inert = reference_saturation_mask(&map, SW, SH, f64::INFINITY);
        assert!(
            bits(&inert).iter().all(|&b| !b),
            "a non-finite threshold sets nothing"
        );
    }

    /// T6 (masked mode): a project reference's frames are NaN outside their
    /// coverage, so `R` is NaN there and no bit is ever set — even at a
    /// threshold every finite ratio clears.
    #[test]
    fn nan_outside_coverage_never_sets_a_bit() {
        let mut map = vec![f32::NAN; SW * SH];
        for (i, cover) in [40usize, 56].into_iter().enumerate() {
            let mut p = plane_with_core(20 + i as u64, 20, 20, 0.7);
            for (k, v) in p.iter_mut().enumerate() {
                if k % SW >= cover {
                    *v = f32::NAN;
                }
            }
            accumulate_reference_saturation(&mut map, &p, SW, SH, None);
        }
        let mask = reference_saturation_mask(&map, SW, SH, 0.0);
        for (k, b) in bits(&mask).into_iter().enumerate() {
            let covered = k % SW < 56;
            assert_eq!(b, covered, "pixel {k}: R = {}", map[k]);
            assert_eq!(map[k].is_nan(), !covered, "pixel {k}");
        }
    }

    /// T6 through the run's builder: the `empty_as_nan` fixture's members
    /// are NaN past each plane's own cover, read through the same warp the
    /// reference integrates; per channel the map is NaN exactly where no
    /// member covers and the mask never sets a bit there.
    #[test]
    fn the_builder_leaves_each_planes_uncovered_pixels_nan_and_unmasked() {
        const W: usize = 96;
        const H: usize = 64;
        const C: usize = 3;
        const COVER: [usize; C] = [40, 60, 80];
        let dir = tempfile::tempdir().unwrap();
        let mut frames = Vec::new();
        for i in 0..4usize {
            let mut data = Vec::with_capacity(W * H * C);
            for (p, &cover) in COVER.iter().enumerate() {
                let mut plane = vec![0.1f32 + 0.05 * p as f32; W * H];
                add_noise(&mut plane, 0.003, 4000 + (i * C + p) as u64);
                for (k, v) in plane.iter_mut().enumerate() {
                    if k % W >= cover {
                        *v = f32::NAN;
                    }
                }
                data.extend_from_slice(&plane);
            }
            let path = dir.path().join(format!("cov{i}.fits"));
            write_fits_f32(&path, W, H, C, &data, &[]).unwrap();
            frames.push(StackFrame {
                path,
                map: identity_map(),
                measurement: FrameMeasurement {
                    width: W,
                    height: H,
                    channels: vec![ChannelMeasurement::default(); C],
                    duration_ms: 0,
                },
                weight: weight(1.0),
                exposure_s: 60.0,
                date_obs: None,
                frame_id: 900300 + i as i64,
                registered_path: None,
            });
        }
        let integration = IntegrationConfig::default();
        let normalization = NormalizationConfig::default();
        let input = GroupInput {
            frames: &frames,
            reference: 0,
            width: W,
            height: H,
            channels: C,
            interpolation: Interpolation::Bilinear,
            clamping: 0.3,
            integration: &integration,
            normalization: &normalization,
            ln: None,
            rej: None,
            second_pass_bitmaps: false,
            coverage_map: false,
            stats_on_covered: false,
        };
        let maps =
            build_reference_saturation(&input, &[0, 1, 2, 3], &pool(), &AtomicBool::new(false))
                .unwrap();
        assert_eq!(maps.len(), C);
        for (p, map) in maps.iter().enumerate() {
            let mask = reference_saturation_mask(map, W, H, 0.0);
            for (k, &r) in map.iter().enumerate() {
                // The bilinear edge column: see
                // `empty_as_nan_marks_each_planes_own_uncovered_pixels`.
                let covered = k % W + 1 < COVER[p];
                let bit = mask.get((k % W) as isize, (k / W) as isize);
                if covered {
                    assert!(r.is_finite() && r > 0.0, "plane {p} pixel {k}: {r}");
                    assert!(bit, "plane {p} pixel {k}");
                } else {
                    assert!(r.is_nan(), "plane {p} pixel {k}: {r}");
                    assert!(!bit, "plane {p} pixel {k}");
                }
            }
        }
    }

    /// The builder reads each member exactly as `integrate_planes` does: a
    /// materialized registered artifact verbatim (its calibrated `path` is
    /// never opened), so the map equals folding the artifacts' planes
    /// directly; only `frames_used` contributes; a raised `cancel` stops it.
    #[test]
    fn the_builder_reads_the_members_registered_planes_one_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let mut frames = Vec::new();
        let mut planes = Vec::new();
        for i in 0..4usize {
            // Frame i's star is brighter in its own frame than the others'.
            let mut data = Vec::new();
            let mut chan = Vec::new();
            for c in 0..3usize {
                let p = plane_with_core(
                    500 + (i * 3 + c) as u64,
                    30 + 10 * i,
                    30,
                    0.3 + 0.1 * i as f32,
                );
                data.extend_from_slice(&p);
                chan.push(p);
            }
            let registered = dir.path().join(format!("reg{i}.fits"));
            write_fits_f32(&registered, SW, SH, 3, &data, &[]).unwrap();
            planes.push(chan);
            frames.push(StackFrame {
                path: dir.path().join("never-opened.fits"),
                map: identity_map(),
                measurement: FrameMeasurement {
                    width: SW,
                    height: SH,
                    channels: vec![ChannelMeasurement::default(); 3],
                    duration_ms: 0,
                },
                weight: weight(1.0),
                exposure_s: 60.0,
                date_obs: None,
                frame_id: 900400 + i as i64,
                registered_path: Some(registered),
            });
        }
        let integration = IntegrationConfig::default();
        let normalization = NormalizationConfig::default();
        let input = GroupInput {
            frames: &frames,
            reference: 0,
            width: SW,
            height: SH,
            channels: 3,
            interpolation: Interpolation::Bilinear,
            clamping: 0.3,
            integration: &integration,
            normalization: &normalization,
            ln: None,
            rej: None,
            second_pass_bitmaps: false,
            coverage_map: false,
            stats_on_covered: false,
        };
        let used = [3usize, 1, 2];
        let maps =
            build_reference_saturation(&input, &used, &pool(), &AtomicBool::new(false)).unwrap();
        for c in 0..3 {
            let mut expected = vec![f32::NAN; SW * SH];
            for &i in &used {
                accumulate_reference_saturation(&mut expected, &planes[i][c], SW, SH, None);
            }
            let (a, b): (Vec<u32>, Vec<u32>) = (
                maps[c].iter().map(|v| v.to_bits()).collect(),
                expected.iter().map(|v| v.to_bits()).collect(),
            );
            assert_eq!(a, b, "channel {c}");
        }
        // Frame 0 is not a member: its star is not in the map.
        assert!(maps[0][30 * SW + 30] < 0.75, "{}", maps[0][30 * SW + 30]);

        let err =
            build_reference_saturation(&input, &used, &pool(), &AtomicBool::new(true)).unwrap_err();
        assert!(matches!(err, IntegrationError::Cancelled), "{err:?}");
    }

    /// T4: the sidecar round-trips bit for bit (NaN included), carries the
    /// scanner-skip and caller cards, and a reader expecting another width,
    /// height or channel count refuses it naming both shapes; the masks the
    /// run reads are the thresholded planes.
    #[test]
    fn the_saturation_sidecar_round_trips_and_refuses_another_shape() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (12usize, 8usize);
        let maps: Vec<Vec<f32>> = (0..3)
            .map(|c| {
                (0..w * h)
                    .map(|i| {
                        if i % 7 == c {
                            f32::NAN
                        } else {
                            (i as f32 * 0.013 + c as f32 * 0.2).sin().abs() * 1.1
                        }
                    })
                    .collect()
            })
            .collect();
        let path = dir.path().join("reference-saturation.fits");
        let cards = vec![
            Card::new("ATH_STKI", CardValue::Str("run-7".into())).unwrap(),
            Card::new("ATH_STKG", CardValue::Str("g".into())).unwrap(),
        ];
        let mut owned = maps.clone();
        let masks = write_reference_saturation(&mut owned, w, h, &path, &cards, f64::INFINITY)
            .unwrap();
        assert_eq!(masks.len(), 3);

        let back = read_reference_saturation(&path, w, h, 3).unwrap();
        assert_eq!(back.len(), 3);
        for (a, b) in maps.iter().zip(&back) {
            let (a, b): (Vec<u32>, Vec<u32>) = (
                a.iter().map(|v| v.to_bits()).collect(),
                b.iter().map(|v| v.to_bits()).collect(),
            );
            assert_eq!(a, b);
        }
        let header = FitsHeader::from_path(&path).unwrap();
        assert_eq!(header.get_str("ATH_STK").as_deref(), Some("T"));
        assert_eq!(header.get_str("ATH_STKI").as_deref(), Some("run-7"));
        assert_eq!(header.get_str("ATH_STKG").as_deref(), Some("g"));

        for (ew, eh, ec) in [(w + 1, h, 3), (w, h - 1, 3), (w, h, 1)] {
            let e = read_reference_saturation(&path, ew, eh, ec).unwrap_err();
            let msg = format!("{e:#}");
            assert!(
                msg.contains(&format!("{w}x{h}x3")) && msg.contains(&format!("{ew}x{eh}x{ec}")),
                "{msg}"
            );
            assert!(read_reference_saturation_masks(&path, ew, eh, ec, 0.75).is_err());
        }

        let masks = read_reference_saturation_masks(&path, w, h, 3, 0.75).unwrap();
        assert_eq!(masks.len(), 3);
        for (m, map) in masks.iter().zip(&maps) {
            assert_eq!(m, &reference_saturation_mask(map, w, h, 0.75));
        }
        assert!(read_reference_saturation(&dir.path().join("missing.fits"), w, h, 3).is_err());
    }
}

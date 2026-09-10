//! The LN reference per group (spec §5.2, M2 Task 4): a low-noise,
//! resolution-shared integration of the group's best-weighted included
//! frames, built once and kept in RAM for the LN pass (M2 Task 5 measures
//! per-frame background/scale against it), and optionally written to
//! `ln/<group>/reference.fits` as an artifact ([`write_reference`] /
//! [`read_reference`]).
//!
//! [`build_reference`] shares [`crate::stacking::integrate::integrate_planes`]
//! — the per-plane `RegisteredSource` + `integrate_stack` loop
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
use std::sync::atomic::AtomicBool;

use anyhow::Context;
use tracing::info;

use crate::fits_writer::{write_fits_f32, Card, CardValue, FitsWriteError};
use crate::integration::combine::{Combination, IntegrationRecipe, Rejection};
use crate::integration::engine::EngineProgress;
use crate::integration::io_policy::IoPolicy;
use crate::integration::plane_reader::PlaneReader;
use crate::integration::stats::{OutputNormalization, RejectionNormalization};
use crate::integration::IntegrationError;
use crate::stacking::integrate::{
    integrate_planes, validate_group_input, GroupInput, GroupProgress,
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
#[allow(clippy::too_many_arguments)]
pub fn build_reference(
    input: &GroupInput<'_>,
    included: &[usize],
    n: usize,
    pool: &rayon::ThreadPool,
    cancel: &AtomicBool,
    io: IoPolicy,
    on_progress: &(dyn Fn(usize, usize) + Sync),
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
        included = included.len(),
        used = frames_used.len(),
        reference_frames = n,
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

    let outputs = integrate_planes(
        input,
        &frames_used,
        &weights,
        output_mode,
        rejection_mode,
        recipe,
        write_maps,
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
    let mut cards = vec![
        Card::new("IMAGETYP", CardValue::Str("LN Reference".into()))?,
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

    fn pool() -> rayon::ThreadPool {
        rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap()
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
}

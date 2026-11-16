//! The Analysis tab's measurement: a raw frame measured through the one star path (Plan C, Task C5).
//!
//! The file read, the measurement and the frame arithmetic live in rustafits (`star_measure::raw::read_raw_frame`,
//! `star_measure::analysis_mode::{measure_raw_frame, FrameSummary}`, port plan phase 4); this module keeps what is the
//! app's: [`frame_options`] builds the measurement's options from the Analysis config, and [`map_measurement`] writes a
//! [`FrameSummary`] and the measured stars into the `FrameAnalysis` / `StarMetric` rows (ids, `config_hash`,
//! `analyzed_at`, the detection threshold, the frame's size). Positions stay in FILE row order (unflipped);
//! [`analyze_frame`] returns `flip_vertical` for the overlay.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

use super::config::{AnalysisConfig, AnalysisPsfModel};
use crate::models::{FrameAnalysis, FrameVerdict, StarMetric};
use crate::star_measure::analysis_mode::{self, FrameOptions, FrameSummary, Verdict};
use crate::star_measure::raw::read_raw_frame;
pub use crate::star_measure::raw::{RawFrame, RawPlanes};
use crate::star_measure::{PlaneMeasurement, PsfModel, MEASURE_VERSION};

// The forced-list zero point's composition needs the plate solver's types and `zero_point`, both `solver`-gated; a
// `render`-only build has the Analysis measurement above and none of it.
#[cfg(feature = "solver")]
pub use forced::{measure_with_zero_point, ForcedRun, ZpInputs};

/// The measurement's options for an Analysis config: the three knobs that reach the measurement (`FrameOptions` is
/// `#[non_exhaustive]`, so it is built from its default and the fields assigned).
pub fn frame_options(config: &AnalysisConfig) -> FrameOptions {
    let mut opts = FrameOptions::default();
    opts.detection_threshold = config.detection_threshold;
    opts.psf_model = match config.psf_model {
        AnalysisPsfModel::Moffat4 => PsfModel::Moffat4,
        AnalysisPsfModel::Auto => PsfModel::Auto,
    };
    opts.max_stars = config.max_stars as usize;
    opts
}

impl From<Verdict> for FrameVerdict {
    fn from(v: Verdict) -> Self {
        match v {
            Verdict::Trailed => FrameVerdict::Trailed,
            Verdict::Defocused => FrameVerdict::Defocused,
            Verdict::NoStars => FrameVerdict::NoStars,
        }
    }
}

/// Measure a raw frame (PC-4, PC-5): mono -> `measure_plane`; a Bayer mosaic -> the green sites; a debayered file ->
/// `measure_plane` on its G plane (`colour`, mixed pixel noise, green share 0.5). The work is rustafits'
/// `analysis_mode::measure_raw_frame`.
pub fn measure_raw_frame(
    frame: &RawFrame,
    config: &AnalysisConfig,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> PlaneMeasurement {
    analysis_mode::measure_raw_frame(frame, &frame_options(config), pool)
}

/// [`measure_raw_frame`] with the module's per-seed diagnostics (`diagnostics`) for the probe's refusal lines; the
/// numbers are the same.
pub fn measure_raw_frame_with(
    frame: &RawFrame,
    config: &AnalysisConfig,
    pool: Option<&Arc<rayon::ThreadPool>>,
    diagnostics: bool,
) -> PlaneMeasurement {
    analysis_mode::measure_raw_frame_with(frame, &frame_options(config), pool, diagnostics)
}

/// The measured stars as `StarMetric` rows (ids still unset): what [`map_measurement`] stores and what the zero point's
/// anchor reads. A star's non-finite `snr` / `hfr` is `None` (PC-8).
pub fn star_metrics(pm: &PlaneMeasurement) -> Vec<StarMetric> {
    pm.stars
        .iter()
        .map(|f| StarMetric {
            id: None,
            frame_analysis_id: 0,
            x: f.x,
            y: f.y,
            peak: f.amplitude,
            flux: f.signal,
            fwhm: f.fwhm(),
            fwhm_x: f.fwhm_x,
            fwhm_y: f.fwhm_y,
            eccentricity: f.eccentricity(),
            snr: f.snr.is_finite().then_some(f.snr),
            hfr: f.hfr.is_finite().then_some(f.hfr),
            theta: f.theta,
            beta: Some(f.beta),
            fit_method: "moffat".to_string(),
            fit_residual: f.residual,
        })
        .collect()
}

/// Write a measurement into the Analysis rows (spec 4.2 with PC-6..PC-9).
///
/// The frame figures are [`FrameSummary::from_measurement`]'s: medians read 0.0 when the frame has no fit in its
/// statistic (PC-7); `snr_weight`, `psf_signal` and `trail_r_squared` read the same fits. `median_beta` is the frame's
/// fitted beta under `Auto` and NULL otherwise (PC-9). A star's non-finite `snr` / `hfr` is `None` (PC-8). The row
/// carries the measurement version, the refusal count and the frame verdict (PC-13): trailed / defocused as the
/// module judged, else no_stars when no fit stands in the frame statistic, else none.
pub fn map_measurement(
    pm: &PlaneMeasurement,
    frame: &RawFrame,
    config: &AnalysisConfig,
    config_hash: &str,
) -> (FrameAnalysis, Vec<StarMetric>) {
    let stars = star_metrics(pm);

    let s = FrameSummary::from_measurement(pm, &frame_options(config));

    let analysis = FrameAnalysis {
        id: None,
        frame_id: 0,
        file_id: 0,
        stars_detected: s.stars_detected as i64,
        median_fwhm: s.median_fwhm,
        median_eccentricity: s.median_eccentricity,
        median_snr: s.median_snr,
        median_hfr: s.median_hfr,
        frame_snr: s.frame_snr,
        snr_weight: s.snr_weight,
        psf_signal: s.psf_signal,
        background: s.background,
        noise: s.noise,
        detection_threshold: config.detection_threshold,
        width: frame.w as i64,
        height: frame.h as i64,
        source_channels: frame.channels as i64,
        trail_r_squared: s.trail_r_squared,
        possibly_trailed: pm.trailed,
        median_beta: s.median_beta,
        quality_score: None,
        config_hash: Some(config_hash.to_string()),
        analyzed_at: chrono::Utc::now().to_rfc3339(),
        measure_version: Some(MEASURE_VERSION),
        stars_refused: Some(s.stars_refused as i64),
        frame_verdict: s.verdict.map(FrameVerdict::from),
        zero_point: None,
        zp_stars: None,
        zp_sigma: None,
        zp_version: None,
        zp_catalog: None,
    };
    (analysis, stars)
}

/// Analyze an already-read frame (CPU only, so the batch keeps its read overlap): measure, then map.
pub fn analyze_raw_frame(
    frame: &RawFrame,
    config: &AnalysisConfig,
    pool: Option<&Arc<rayon::ThreadPool>>,
    config_hash: &str,
) -> (FrameAnalysis, Vec<StarMetric>) {
    let pm = measure_raw_frame(frame, config, pool);
    map_measurement(&pm, frame, config, config_hash)
}

/// Analyze a single frame file (reads it with [`read_raw_frame`]). Returns the frame's `flip_vertical` for the overlay;
/// the star positions are in file row order.
pub fn analyze_frame(
    path: &str,
    config: &AnalysisConfig,
    pool: Option<&Arc<rayon::ThreadPool>>,
    config_hash: &str,
) -> Result<(FrameAnalysis, Vec<StarMetric>, bool)> {
    let mut frame = read_raw_frame(Path::new(path))?;
    frame.drop_unmeasured_planes();
    let (analysis, stars) = analyze_raw_frame(&frame, config, pool, config_hash);
    Ok((analysis, stars, frame.flip_vertical))
}

// ---------------------------------------------------------------------------
// The forced-list zero point's composition (forced-list spec §5.1-§5.3)
// ---------------------------------------------------------------------------

#[cfg(feature = "solver")]
mod forced {
    use std::any::Any;
    use std::cell::RefCell;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::Arc;

    use anyhow::Result;
    use solvemyastro::{CatalogStar, WcsSolution};

    use super::super::config::AnalysisConfig;
    use super::super::zero_point::{
        anchor_zero_point, forced_list_in, forced_zero_point, map_gate, pixel_scale_arcsec, placement, position_map,
        Anchor, ForcedList, ForcedListInput, ForcedListParams, ForcedOutcome, ForcedRefusal, ForcedZeroPoint, MapGate,
        QuadMap,
    };
    use super::{frame_options, star_metrics, RawFrame, RawPlanes};
    use crate::star_measure::analysis_mode::{
        self, FrameOptions, FrameSummary, ListFit, ListMeasurement, ListPlacement, ListRefusal,
    };
    use crate::star_measure::{Domain, PlaneMeasurement, ADU_SCALE};

    /// What a solved frame hands the zero point: its sky-to-pixel transform, `EXPTIME` and observation epoch (decimal year).
    #[derive(Debug, Clone, Copy)]
    pub struct ZpInputs<'a> {
        pub wcs: &'a WcsSolution,
        pub exptime_s: Option<f64>,
        pub epoch: f64,
    }

    /// What [`measure_with_zero_point`] found beside the measurement: the zero point or its refusal, and every figure
    /// behind it (the probe's rows, the analysis row's stored figures).
    #[derive(Debug, Clone)]
    pub struct ForcedRun {
        /// The zero point, or why there is none ([`ForcedRefusal::is_stored`] says whether the refusal is written).
        pub result: Result<ForcedZeroPoint, ForcedRefusal>,
        /// The frame's preliminary zero point, when the budget gave one.
        pub anchor: Option<Anchor>,
        /// The list and its window, when one was built.
        pub list: Option<ForcedList>,
        /// What the fitter measured at the list's positions.
        pub measured: Option<ListMeasurement>,
        /// The frame's map of its own fits, when the map stars' fits gave one.
        pub map: Option<QuadMap>,
        /// What the second fit said of the map, when it was asked.
        pub gate: Option<MapGate>,
        /// Where the refits and the apertures were placed, when the map gave a placement.
        pub placement: Option<ListPlacement>,
        /// The statistic with its per-star rows, when the list was measured.
        pub outcome: Option<ForcedOutcome>,
    }

    impl ForcedRun {
        fn refused(r: ForcedRefusal) -> Self {
            ForcedRun {
                result: Err(r),
                anchor: None,
                list: None,
                measured: None,
                map: None,
                gate: None,
                placement: None,
                outcome: None,
            }
        }
    }

    #[cfg(test)]
    thread_local! {
        /// A test's switch: the placement callback panics on this thread (the catch is otherwise out of reach).
        pub(super) static PLACEMENT_PANICS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        /// The same for the gate and the statistic, which run after the fitter returned.
        pub(super) static STATISTIC_PANICS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// The fit's working-unit full scale, as the saturation test reads it (`saturation_plane`'s `norm`).
    fn full_scale_working(domain: &Domain) -> f64 {
        match domain {
            Domain::Calibrated { .. } => f64::from(ADU_SCALE),
            Domain::RawAdu { full_scale, .. } => *full_scale,
        }
    }

    /// The text of a caught panic's payload.
    fn panic_text(payload: &(dyn Any + Send)) -> String {
        payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "a panic with no text".to_string())
    }

    /// The part of the composition that runs on the measurement: the plane's verdict, the exposure, the cone, the anchor
    /// and the list. `Err` is the frame's refusal. A panic of `cone` is caught here: it is a failure of this run
    /// (`CatalogueUnreadable`, nothing stored), never the loss of the Analysis measurement the caller already holds.
    fn prepare_forced_list<F>(
        frame: &RawFrame,
        pm: &PlaneMeasurement,
        opts: &FrameOptions,
        zp: &ZpInputs,
        params: &ForcedListParams,
        pool: Option<&Arc<rayon::ThreadPool>>,
        cone: F,
    ) -> Result<(Anchor, ForcedList, f64), ForcedRefusal>
    where
        F: FnOnce(&WcsSolution, usize, usize, f64, f64) -> Result<Vec<CatalogStar>>,
    {
        // A plane with no stars in its statistic gives nothing a re-read would change (trailed and defocused planes never
        // reach the callback).
        let summary = FrameSummary::from_measurement(pm, opts);
        if summary.verdict.is_some() {
            return Err(ForcedRefusal::FrameVerdict);
        }
        let exptime = match zp.exptime_s {
            Some(t) if t.is_finite() && t > 0.0 => t,
            _ => return Err(ForcedRefusal::NoExposure),
        };
        let scale = pixel_scale_arcsec(zp.wcs);
        let (cx, cy) = ((frame.w as f64 - 1.0) / 2.0, (frame.h as f64 - 1.0) / 2.0);
        let (ra, dec) = zp.wcs.pixel_to_sky(cx, cy);
        if !(scale.is_finite() && scale > 0.0 && ra.is_finite() && dec.is_finite()) {
            return Err(ForcedRefusal::UnreadableSolve);
        }
        let read = catch_unwind(AssertUnwindSafe(|| cone(zp.wcs, frame.w, frame.h, params.region_fraction, zp.epoch)));
        let catalog = match read {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "forced list: the catalogue cone could not be read");
                return Err(ForcedRefusal::CatalogueUnreadable);
            }
            Err(payload) => {
                tracing::error!(error = %panic_text(&*payload), "forced list: the catalogue cone panicked");
                return Err(ForcedRefusal::CatalogueUnreadable);
            }
        };
        let rows = star_metrics(pm);
        let anchor = anchor_zero_point(
            &rows,
            pm.reported_aperture_correction,
            zp.wcs,
            &catalog,
            Some(exptime),
            frame.w,
            frame.h,
            pm.frame.fwhm_px.unwrap_or(0.0),
            params,
        )?;
        let list = forced_list_in(
            &ForcedListInput {
                catalog: &catalog,
                wcs: zp.wcs,
                width: frame.w,
                height: frame.h,
                anchor: anchor.zero_point,
                full_scale: full_scale_working(&frame.domain),
                exptime_s: exptime,
            },
            params,
            pool,
        )?;
        // `F`, the Analysis median FWHM: the aperture and its sky annulus grow with it.
        Ok((anchor, list, summary.median_fwhm))
    }

    /// Measure a raw frame exactly as [`super::measure_raw_frame`] does and, when the frame is solved (`inputs`), its
    /// forced-list zero point: the Analysis measurement, then the anchor from its own fits, the list from the catalogue,
    /// the map stars fitted on the same plane (rustafits' list entry), the frame's map of their offsets, a plain
    /// aperture at every list star's moved position and the statistic over those sums. Pure of any database or file; the
    /// probe and the analyzer call this one function, so what the probe judges is what ships. `cone` reads the catalogue
    /// stars (`wcs`, width, height, region fraction, epoch) and is called at most once; a trailed or defocused plane and
    /// a three-plane file never call it. The Analysis numbers are bit-identical with and without `inputs`. Nothing in the
    /// zero-point half loses them: a cone that errors or panics is `catalogue_unreadable` (not stored), and any other
    /// panic of the composition is `internal` (not stored).
    pub fn measure_with_zero_point<F>(
        frame: &RawFrame,
        config: &AnalysisConfig,
        pool: Option<&Arc<rayon::ThreadPool>>,
        inputs: Option<&ZpInputs>,
        params: &ForcedListParams,
        cone: F,
    ) -> (PlaneMeasurement, Option<ForcedRun>)
    where
        F: FnOnce(&WcsSolution, usize, usize, f64, f64) -> Result<Vec<CatalogStar>>,
    {
        let opts = frame_options(config);
        let Some(zp) = inputs else {
            return (analysis_mode::measure_raw_frame(frame, &opts, pool), None);
        };
        // Both callbacks run inside one call and share what they find, so the state sits in cells.
        let refused: RefCell<Option<ForcedRefusal>> = RefCell::new(None);
        let built: RefCell<Option<(Anchor, ForcedList)>> = RefCell::new(None);
        let placed: RefCell<Option<(QuadMap, ListPlacement)>> = RefCell::new(None);
        let frame_fwhm = std::cell::Cell::new(f64::NAN);
        let (pm, measured) = analysis_mode::measure_raw_frame_and_list(
            frame,
            &opts,
            pool,
            |pm| {
                match catch_unwind(AssertUnwindSafe(|| prepare_forced_list(frame, pm, &opts, zp, params, pool, cone))) {
                    Ok(Ok((anchor, list, fwhm))) => {
                        frame_fwhm.set(fwhm);
                        // No map star: nothing to fit (the refusal is the list's size, below).
                        let request = (!list.map.is_empty()).then(|| list.request.clone());
                        *built.borrow_mut() = Some((anchor, list));
                        request
                    }
                    Ok(Err(r)) => {
                        *refused.borrow_mut() = Some(r);
                        None
                    }
                    Err(payload) => {
                        tracing::error!(error = %panic_text(&*payload), "forced list: the composition panicked");
                        *refused.borrow_mut() = Some(ForcedRefusal::Internal);
                        None
                    }
                }
            },
            |fits: &[ListFit]| {
                let outcome = catch_unwind(AssertUnwindSafe(|| {
                    #[cfg(test)]
                    if PLACEMENT_PANICS.with(|c| c.get()) {
                        panic!("the placement panicked (test)");
                    }
                    let built = built.borrow();
                    let (_, list) = built.as_ref().ok_or(ForcedRefusal::Internal)?;
                    let map = position_map(list, fits, params)?;
                    let placement = placement(list, &map, frame_fwhm.get(), params);
                    Ok::<_, ForcedRefusal>((map, placement))
                }));
                match outcome {
                    Ok(Ok((map, p))) => {
                        *placed.borrow_mut() = Some((map, p.clone()));
                        Some(p)
                    }
                    Ok(Err(r)) => {
                        *refused.borrow_mut() = Some(r);
                        None
                    }
                    Err(payload) => {
                        tracing::error!(error = %panic_text(&*payload), "forced list: the placement panicked");
                        *refused.borrow_mut() = Some(ForcedRefusal::Internal);
                        None
                    }
                }
            },
        );
        let (refused, built, placed) = (refused.into_inner(), built.into_inner(), placed.into_inner());

        let colour = matches!(frame.planes, RawPlanes::Rgb(_));
        let run = match measured {
            Some(lm) => match lm.refusal {
                Some(ListRefusal::ColourFrame) => {
                    let mut run = ForcedRun::refused(ForcedRefusal::ColourCamera);
                    run.measured = Some(lm);
                    run
                }
                Some(other) => {
                    tracing::warn!(reason = other.code(), "forced list was refused by the fitter for a reason this build does not know");
                    let mut run = ForcedRun::refused(ForcedRefusal::Internal);
                    run.measured = Some(lm);
                    run
                }
                None => match built {
                    Some((anchor, list)) => {
                        let mut run = ForcedRun {
                            result: Err(ForcedRefusal::Internal),
                            anchor: Some(anchor),
                            list: Some(list),
                            measured: None,
                            map: None,
                            gate: None,
                            placement: None,
                            outcome: None,
                        };
                        let list_ref = run.list.as_ref().expect("just set");
                        match (refused, placed) {
                            // The map's own refusal (too few members, or a refusal of the second callback).
                            (Some(r), _) => run.result = Err(r),
                            (None, Some((map, placement))) => {
                                // The gate and the statistic are pure but not exempt from a bug: a panic here must not
                                // lose the Analysis measurement either (`pm` is returned whatever the verdict).
                                let composed = catch_unwind(AssertUnwindSafe(|| {
                                    #[cfg(test)]
                                    if STATISTIC_PANICS.with(|c| c.get()) {
                                        panic!("the statistic panicked (test)");
                                    }
                                    let gate = map_gate(list_ref, &lm.refits, &placement, params)?;
                                    // `exptime_s` was valid to get here.
                                    let t = zp.exptime_s.unwrap_or(f64::NAN);
                                    Ok::<_, ForcedRefusal>((gate, forced_zero_point(list_ref, &lm, &placement, t, params)))
                                }));
                                match composed {
                                    Ok(Ok((gate, outcome))) => {
                                        run.result = outcome.result;
                                        run.gate = Some(gate);
                                        run.outcome = Some(outcome);
                                    }
                                    Ok(Err(r)) => run.result = Err(r),
                                    Err(payload) => {
                                        tracing::error!(
                                            error = %panic_text(&*payload),
                                            "forced list: the gate or the statistic panicked"
                                        );
                                        run.result = Err(ForcedRefusal::Internal);
                                    }
                                }
                                run.map = Some(map);
                                run.placement = Some(placement);
                            }
                            (None, None) => {
                                tracing::warn!("forced list was measured with no map and no refusal");
                            }
                        }
                        run.measured = Some(lm);
                        run
                    }
                    None => {
                        tracing::warn!("forced list measured without a list built");
                        ForcedRun::refused(ForcedRefusal::Internal)
                    }
                },
            },
            None => {
                // The layout first: rustafits returns `None` for a trailed or defocused plane before it looks at the
                // layout, and a three-plane file is a colour camera whatever its plane looks like.
                let reason = if colour {
                    ForcedRefusal::ColourCamera
                } else if let Some(r) = refused {
                    r
                } else if pm.trailed || pm.defocused {
                    ForcedRefusal::FrameVerdict
                } else if built.as_ref().is_some_and(|(_, l)| l.map.is_empty()) {
                    // No map star: nothing in the window, or the catalogue holds nothing there. The refusal carries the
                    // list's size, so a shallow catalogue is told from a measurement that failed.
                    let n = built.as_ref().map_or(0, |(_, l)| l.stars.len() as u32);
                    ForcedRefusal::TooFewStars { list: n, kept: 0 }
                } else {
                    // A non-empty, valid request the fitter returned nothing for: its tolerance or the plane's mask
                    // geometry broke an invariant. A bug, not a verdict: not stored, a fixed build measures again.
                    let n = built.as_ref().map_or(0, |(_, l)| l.map.len());
                    tracing::warn!(list = n, "forced list was not measured: the request or the plane's mask was refused");
                    ForcedRefusal::Internal
                };
                let (anchor, list) = match built {
                    Some((a, l)) => (Some(a), Some(l)),
                    None => (None, None),
                };
                let mut run = ForcedRun::refused(reason);
                run.anchor = anchor;
                run.list = list;
                run
            }
        };
        if run.list.is_some() {
            let refusal = run.result.err().map(|r| r.code());
            tracing::debug!(
                list = run.list.as_ref().map_or(0, |l| l.stars.len()),
                members = run.outcome.as_ref().map_or(0, |o| o.members as usize),
                map_members = run.map.as_ref().map_or(0, |m| m.members),
                map_kept_share = run.gate.as_ref().map_or(f64::NAN, |g| g.kept_share),
                map_residual_px = run.map.as_ref().map_or(f64::NAN, |m| m.residual_px),
                pedestal = run.outcome.as_ref().and_then(|o| o.pedestal).unwrap_or(f64::NAN),
                window_slope = run.outcome.as_ref().and_then(|o| o.window_slope).unwrap_or(f64::NAN),
                zp_reason = refusal,
                "forced list measured"
            );
        }
        (pm, Some(run))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::star_measure::Domain;
    use crate::test_support::{add_noise, moffat_field, MoffatStar};
    use astroimage::BayerPattern;

    const RAW: Domain = Domain::RawAdu {
        clip: 65535.0,
        full_scale: 65535.0,
        gain_e_per_adu: None,
    };

    fn star(x: f64, y: f64, amp: f64, fwhm: f64) -> MoffatStar {
        // Moffat beta 4: FWHM = 2 alpha sqrt(2^(1/4) - 1).
        let alpha = fwhm / (2.0 * (2f64.powf(0.25) - 1.0).sqrt());
        MoffatStar {
            x,
            y,
            amp,
            alpha_x: alpha,
            alpha_y: alpha,
            theta: 0.0,
        }
    }

    /// `n` stars on a jittered grid of a `w` x `h` plane, amplitudes 500..2000.
    fn grid_stars(n: usize, w: usize, h: usize, fwhm: f64, seed: u64) -> Vec<MoffatStar> {
        let mut rng = crate::geometry::ransac::SplitMix64(seed);
        let cols = ((n as f64 * w as f64 / h as f64).sqrt().ceil() as usize).max(1);
        let rows = n.div_ceil(cols);
        let (cw, ch) = (w as f64 / cols as f64, h as f64 / rows as f64);
        (0..n)
            .map(|i| {
                let (c, r) = ((i % cols) as f64, (i / cols) as f64);
                let x = c * cw + 12.0 + rng.next_f64() * (cw - 24.0);
                let y = r * ch + 12.0 + rng.next_f64() * (ch - 24.0);
                star(x, y, 500.0 + rng.next_f64() * 1500.0, fwhm)
            })
            .collect()
    }

    fn mono_frame(n: usize, w: usize, h: usize, sky: f32, seed: u64) -> RawFrame {
        let stars = grid_stars(n, w, h, 3.5, seed);
        let mut p = moffat_field(w, h, &stars, 4.0, sky);
        add_noise(&mut p, 5.0, seed + 1);
        RawFrame {
            planes: RawPlanes::Mono(p),
            w,
            h,
            domain: RAW,
            flip_vertical: false,
            channels: 1,
        }
    }

    fn cfg() -> AnalysisConfig {
        AnalysisConfig::default()
    }

    fn all_finite(a: &FrameAnalysis) {
        for (name, v) in [
            ("median_fwhm", a.median_fwhm),
            ("median_eccentricity", a.median_eccentricity),
            ("median_snr", a.median_snr),
            ("median_hfr", a.median_hfr),
            ("frame_snr", a.frame_snr),
            ("snr_weight", a.snr_weight),
            ("psf_signal", a.psf_signal),
            ("background", a.background),
            ("noise", a.noise),
            ("detection_threshold", a.detection_threshold),
            ("trail_r_squared", a.trail_r_squared),
        ] {
            assert!(v.is_finite(), "{name} = {v}");
        }
    }

    #[test]
    fn analysis_of_a_synthetic_mono_frame_maps_every_column() {
        let frame = mono_frame(300, 1024, 768, 1000.0, 7);
        let (a, stars) = analyze_raw_frame(&frame, &cfg(), None, "h");
        all_finite(&a);
        assert!(
            (a.median_fwhm - 3.5).abs() < 0.07,
            "median_fwhm {}",
            a.median_fwhm
        );
        assert!(a.median_eccentricity < 0.1, "ecc {}", a.median_eccentricity);
        assert!(a.stars_detected >= 285, "detected {}", a.stars_detected);
        assert!(!stars.is_empty());
        assert!(stars.iter().all(|s| s.fit_method == "moffat"));
        assert_eq!(a.frame_snr, a.background / a.noise);
        assert!((a.background - 1000.0).abs() < 10.0, "bg {}", a.background);
        assert!(a.snr_weight > 0.0 && a.psf_signal > 0.0);
        assert_eq!((a.width, a.height, a.source_channels), (1024, 768, 1));
        assert_eq!(a.detection_threshold, 6.0);
        assert_eq!(a.config_hash.as_deref(), Some("h"));
        assert!(a.median_beta.is_none());
        assert!(!a.possibly_trailed);
        for s in &stars {
            assert!(s.fwhm_x >= s.fwhm_y && s.eccentricity >= 0.0 && s.eccentricity < 1.0);
            assert!(s.x.is_finite() && s.y.is_finite() && s.peak > 0.0 && s.flux > 0.0);
        }
    }

    /// R-PC-5 / R-PC-8: `stars_detected` counts the matched filter's point-like seeds. A 3F Gaussian knot is a seed the
    /// module flags `extended`: it is fitted but is not counted, in every layout (mono, a mosaic's green sites, a
    /// debayered file's G plane), so the count with the knot equals the count without it.
    #[test]
    fn stars_detected_leaves_knots_out_in_every_layout() {
        use crate::test_support::gaussian_field;
        let (w, h) = (320, 240);
        let stars = grid_stars(40, w, h, 3.5, 21);
        let knot = (160.3, 120.8);
        let g = gaussian_field(w, h, &[(knot.0, knot.1, 400.0)], 3.0 * 3.5 / 2.354_820_045, 0.0);
        let plane = |with_knot: bool, seed: u64| {
            let mut p = moffat_field(w, h, &stars, 4.0, 1000.0);
            if with_knot {
                for (d, b) in p.iter_mut().zip(&g) {
                    *d += b;
                }
            }
            add_noise(&mut p, 6.0, seed);
            p
        };
        let frame = |planes: RawPlanes, channels| RawFrame {
            planes,
            w,
            h,
            domain: RAW,
            flip_vertical: false,
            channels,
        };
        let make = |name: &str, k: bool| match name {
            "mono" => frame(RawPlanes::Mono(plane(k, 5)), 1),
            "cfa" => frame(
                RawPlanes::Cfa {
                    mosaic: plane(k, 5),
                    pattern: BayerPattern::Rggb,
                },
                1,
            ),
            _ => frame(
                RawPlanes::Rgb([plane(false, 3), plane(k, 5), plane(false, 4)]),
                3,
            ),
        };
        for name in ["mono", "cfa", "rgb"] {
            let (plain, knotted) = (make(name, false), make(name, true));
            let pm = measure_raw_frame(&knotted, &cfg(), None);
            assert!(pm.extended_seeds >= 1, "{name}: the knot is flagged extended");
            if name == "cfa" {
                assert!(pm.detected.is_empty(), "{name}: handed seeds leave `detected` empty");
            } else {
                assert_eq!(pm.detected.len(), pm.seeds, "{name}");
            }
            let (a, _) = analyze_raw_frame(&plain, &cfg(), None, "h");
            let (b, _) = analyze_raw_frame(&knotted, &cfg(), None, "h");
            assert_eq!(b.stars_detected, (pm.seeds - pm.extended_seeds) as i64, "{name}");
            assert_eq!(b.stars_detected, a.stars_detected, "{name}: the knot is not counted");
            assert!(b.stars_detected >= 30, "{name}: {}", b.stars_detected);
        }
    }

    #[test]
    fn a_frame_without_fits_writes_zero_medians() {
        let (w, h) = (400, 300);
        let mut noise = vec![1000.0f32; w * h];
        add_noise(&mut noise, 10.0, 3);
        let donut: Vec<f32> = crate::test_support::donut_field(400, 200, 5)
            .iter()
            .map(|v| v * 65535.0)
            .collect();
        for (planes, w, h) in [(noise, w, h), (donut, 400, 200)] {
            let frame = RawFrame {
                planes: RawPlanes::Mono(planes),
                w,
                h,
                domain: RAW,
                flip_vertical: false,
                channels: 1,
            };
            let (a, stars) = analyze_raw_frame(&frame, &cfg(), None, "h");
            all_finite(&a);
            assert!(stars.is_empty());
            assert_eq!(
                (a.median_fwhm, a.median_eccentricity, a.median_snr, a.median_hfr),
                (0.0, 0.0, 0.0, 0.0)
            );
        }
    }

    #[test]
    fn a_trailed_frame_is_flagged_with_no_measured_star() {
        let p: Vec<f32> = crate::test_support::trailed_field(600, 450, 9)
            .iter()
            .map(|v| v * 65535.0)
            .collect();
        let frame = RawFrame {
            planes: RawPlanes::Mono(p),
            w: 600,
            h: 450,
            domain: RAW,
            flip_vertical: false,
            channels: 1,
        };
        let (a, stars) = analyze_raw_frame(&frame, &cfg(), None, "h");
        all_finite(&a);
        assert!(a.possibly_trailed);
        assert!(stars.is_empty());
        assert_eq!(
            (a.median_fwhm, a.median_eccentricity, a.median_snr, a.median_hfr),
            (0.0, 0.0, 0.0, 0.0)
        );
        assert_eq!(a.trail_r_squared, 0.0);
    }

    fn mono(p: Vec<f32>, w: usize, h: usize) -> RawFrame {
        RawFrame {
            planes: RawPlanes::Mono(p),
            w,
            h,
            domain: RAW,
            flip_vertical: false,
            channels: 1,
        }
    }

    #[test]
    fn the_analyzer_fills_measure_version_and_stars_refused() {
        let scaled = |p: Vec<f32>| p.iter().map(|v| v * 65535.0).collect::<Vec<f32>>();
        let noise = {
            let mut n = vec![1000.0f32; 400 * 300];
            add_noise(&mut n, 10.0, 3);
            mono(n, 400, 300)
        };
        let trailed = mono(scaled(crate::test_support::trailed_field(600, 450, 9)), 600, 450);
        let clean = mono_frame(120, 640, 480, 1000.0, 11);
        for frame in [&noise, &trailed, &clean] {
            let pm = measure_raw_frame(frame, &cfg(), None);
            let (a, _) = map_measurement(&pm, frame, &cfg(), "h");
            assert_eq!(a.measure_version, Some(MEASURE_VERSION));
            // The sum of the refusal reasons; the budget's not-selected / trimmed are not in it.
            assert_eq!(a.stars_refused, Some(pm.refused.by_reason.values().sum::<usize>() as i64));
        }
        let pm = measure_raw_frame(&trailed, &cfg(), None);
        let (a, _) = map_measurement(&pm, &trailed, &cfg(), "h");
        assert!(a.stars_refused.unwrap() > 0, "a trailed frame's fits are refused: {:?}", a.stars_refused);
    }

    /// C6 review m5: with `max_stars` below the seed count the budget leaves seeds `not_selected` (and may trim);
    /// `stars_refused` is the refusal reasons alone, so neither leaks into it.
    #[test]
    fn stars_refused_leaves_the_budgets_not_selected_out() {
        let frame = mono_frame(120, 640, 480, 1000.0, 11);
        let config = AnalysisConfig { max_stars: 20, ..cfg() };
        let pm = measure_raw_frame(&frame, &config, None);
        assert!(pm.refused.not_selected > 0, "premise: the budget left seeds unselected: {:?}", pm.refused);
        let (a, _) = map_measurement(&pm, &frame, &config, "h");
        assert_eq!(a.stars_refused, Some(pm.refused.total() as i64));
        assert_eq!(a.stars_refused, Some(pm.refused.by_reason.values().sum::<usize>() as i64));
    }

    #[test]
    fn a_non_finite_star_snr_or_hfr_maps_to_none() {
        let frame = mono_frame(60, 512, 384, 1000.0, 13);
        let mut pm = measure_raw_frame(&frame, &cfg(), None);
        assert!(pm.stars.len() >= 3);
        pm.stars[0].snr = f64::NAN;
        pm.stars[1].hfr = f64::INFINITY;
        let (_, stars) = map_measurement(&pm, &frame, &cfg(), "h");
        assert_eq!((stars[0].snr, stars[0].hfr.is_some()), (None, true));
        assert_eq!((stars[1].hfr, stars[1].snr.is_some()), (None, true));
        assert!(stars[2].snr.is_some() && stars[2].hfr.is_some());
    }

    #[test]
    fn the_analyzer_records_the_frame_verdict() {
        let (w, h) = (400, 300);
        let mut n = vec![1000.0f32; w * h];
        add_noise(&mut n, 10.0, 3);
        let donut: Vec<f32> = crate::test_support::donut_field(400, 200, 5)
            .iter()
            .map(|v| v * 65535.0)
            .collect();
        let trailed: Vec<f32> = crate::test_support::trailed_field(600, 450, 9)
            .iter()
            .map(|v| v * 65535.0)
            .collect();
        let verdict = |f: RawFrame| analyze_raw_frame(&f, &cfg(), None, "h").0;
        let t = verdict(mono(trailed, 600, 450));
        assert_eq!((t.frame_verdict, t.possibly_trailed), (Some(FrameVerdict::Trailed), true));
        assert_eq!(verdict(mono(donut, 400, 200)).frame_verdict, Some(FrameVerdict::Defocused));
        assert_eq!(verdict(mono(n, w, h)).frame_verdict, Some(FrameVerdict::NoStars));
        assert_eq!(verdict(mono_frame(120, 640, 480, 1000.0, 11)).frame_verdict, None);
    }

    #[test]
    fn moffat4_writes_no_median_beta_and_auto_writes_the_fitted_one() {
        let frame = mono_frame(120, 640, 480, 1000.0, 11);
        let (m4, _) = analyze_raw_frame(&frame, &cfg(), None, "h");
        assert_eq!(m4.median_beta, None);
        let auto = AnalysisConfig {
            psf_model: AnalysisPsfModel::Auto,
            ..cfg()
        };
        let (a, _) = analyze_raw_frame(&frame, &auto, None, "h");
        let b = a.median_beta.expect("Auto writes the fitted beta");
        assert!(b.is_finite() && (b - 4.0).abs() < 1.0, "beta {b}");
    }

    fn site(p: BayerPattern, x: usize, y: usize) -> char {
        let (ex, ey) = (x % 2 == 0, y % 2 == 0);
        match (p, ex, ey) {
            (BayerPattern::Rggb, true, true) => 'R',
            (BayerPattern::Rggb, false, false) => 'B',
            _ => 'G',
        }
    }

    #[test]
    fn an_osc_mosaic_is_measured_on_green_and_a_three_plane_file_on_g() {
        // The mosaic: scene stars (flux 1.0 on G) x site gains + sky per site + noise.
        let (w, h) = (256, 256);
        let truth = (100.3, 80.7);
        let mut stars = vec![star(truth.0, truth.1, 3000.0, 4.0)];
        let mut rng = crate::geometry::ransac::SplitMix64(11);
        while stars.len() < 41 {
            let x = 16.0 + rng.next_f64() * (w as f64 - 32.0);
            let y = 16.0 + rng.next_f64() * (h as f64 - 32.0);
            if stars.iter().any(|s| (s.x - x).hypot(s.y - y) < 20.0) {
                continue;
            }
            stars.push(star(x, y, 250.0 + rng.next_f64() * 500.0, 3.6 + rng.next_f64() * 0.8));
        }
        let scene = moffat_field(w, h, &stars, 4.0, 0.0);
        let mut m: Vec<f32> = (0..w * h)
            .map(|i| {
                let (gain, sky) = match site(BayerPattern::Rggb, i % w, i / w) {
                    'R' => (0.5, 900.0),
                    'B' => (0.4, 800.0),
                    _ => (1.0, 1000.0),
                };
                scene[i] * gain + sky
            })
            .collect();
        add_noise(&mut m, 6.0, 5);
        let frame = RawFrame {
            planes: RawPlanes::Cfa {
                mosaic: m,
                pattern: BayerPattern::Rggb,
            },
            w,
            h,
            domain: RAW,
            flip_vertical: false,
            channels: 1,
        };
        let (a, ms) = analyze_raw_frame(&frame, &cfg(), None, "h");
        all_finite(&a);
        let best = ms
            .iter()
            .min_by(|p, q| {
                (p.x - truth.0)
                    .hypot(p.y - truth.1)
                    .total_cmp(&(q.x - truth.0).hypot(q.y - truth.1))
            })
            .expect("a measured star");
        let d = (best.x - truth.0).hypot(best.y - truth.1);
        assert!(d < 0.1, "bright star {d} px off");
        assert_eq!(a.source_channels, 1);
        assert!(a.stars_detected >= ms.len() as i64 && a.stars_detected >= 35);

        // Three planes: 40 common stars, one only in G, one only in R.
        let (w, h) = (320, 240);
        let common = grid_stars(40, w, h, 3.5, 21);
        // Two free spots, 25 px from every common star and from each other.
        let mut free: Vec<(f64, f64)> = Vec::new();
        let mut rng = crate::geometry::ransac::SplitMix64(77);
        while free.len() < 2 {
            let (x, y) = (20.0 + rng.next_f64() * 280.0, 20.0 + rng.next_f64() * 200.0);
            if common
                .iter()
                .map(|s| (s.x, s.y))
                .chain(free.iter().copied())
                .all(|(a, b)| (a - x).hypot(b - y) > 25.0)
            {
                free.push((x, y));
            }
        }
        let g_only = star(free[0].0, free[0].1, 1500.0, 3.5);
        let r_only = star(free[1].0, free[1].1, 1500.0, 3.5);
        let plane = |extra: Option<MoffatStar>, seed| {
            let mut s = common.clone();
            s.extend(extra);
            let mut p = moffat_field(w, h, &s, 4.0, 1000.0);
            add_noise(&mut p, 10.0, seed);
            p
        };
        let frame = RawFrame {
            planes: RawPlanes::Rgb([plane(Some(r_only), 1), plane(Some(g_only), 2), plane(None, 3)]),
            w,
            h,
            domain: RAW,
            flip_vertical: false,
            channels: 3,
        };
        let (a, ms) = analyze_raw_frame(&frame, &cfg(), None, "h");
        assert_eq!(a.source_channels, 3);
        let near = |s: &MoffatStar| ms.iter().any(|m| (m.x - s.x).hypot(m.y - s.y) < 0.5);
        assert!(near(&g_only), "the G star is measured");
        assert!(!near(&r_only), "the R star is absent");
    }

    // ---- the forced-list composition ----------------------------------------------------------------------------

    #[cfg(feature = "solver")]
    mod forced {
        use super::*;
        use crate::analysis::zero_point::{anchor_zero_point, ForcedListParams, ForcedRefusal};
        use crate::analysis::analyzer::forced::{PLACEMENT_PANICS, STATISTIC_PANICS};
        use solvemyastro::{CatalogStar, WcsSolution};

        const ZP_TRUE: f64 = 24.0;
        const T_EXP: f64 = 10.0;

        fn wcs_1as(w: usize, h: usize) -> WcsSolution {
            let s = 1.0 / 3600.0;
            WcsSolution {
                crpix: ((w as f64 - 1.0) / 2.0, (h as f64 - 1.0) / 2.0),
                crval: (150.0, 20.0),
                cd: [[-s, 0.0], [0.0, s]],
                sip_forward: None,
                sip_reverse: None,
            }
        }

        /// The catalogue that matches `stars`: G from the star's total flux at the true zero point.
        fn catalogue_of(stars: &[MoffatStar], wcs: &WcsSolution) -> Vec<CatalogStar> {
            stars
                .iter()
                .map(|s| {
                    let total = std::f64::consts::PI * s.amp * s.alpha_x * s.alpha_y / 3.0;
                    let g = ZP_TRUE - 2.5 * (total / T_EXP).log10();
                    let (ra, dec) = wcs.pixel_to_sky(s.x, s.y);
                    CatalogStar { ra, dec, mag: g as f32 }
                })
                .collect()
        }

        /// A mono frame of `n` isolated Moffat stars (FWHM 3.5, beta 4) with the catalogue that matches them.
        fn solved_mono(n: usize, w: usize, h: usize, seed: u64) -> (RawFrame, Vec<CatalogStar>, WcsSolution) {
            let stars = grid_stars(n, w, h, 3.5, seed);
            let wcs = wcs_1as(w, h);
            let cat = catalogue_of(&stars, &wcs);
            let mut p = moffat_field(w, h, &stars, 4.0, 1000.0);
            add_noise(&mut p, 5.0, seed + 1);
            let frame = RawFrame { planes: RawPlanes::Mono(p), w, h, domain: RAW, flip_vertical: false, channels: 1 };
            (frame, cat, wcs)
        }

        /// The same scene as an RGGB mosaic: the scene's stars on every site, scaled by the site's gain (green 1.0, red
        /// 0.5, blue 0.4) over the site's own sky, so a fit that read the red or blue sites would be biased.
        fn solved_mosaic(n: usize, w: usize, h: usize, seed: u64) -> (RawFrame, Vec<CatalogStar>, WcsSolution) {
            let stars = grid_stars(n, w, h, 3.5, seed);
            let wcs = wcs_1as(w, h);
            let cat = catalogue_of(&stars, &wcs);
            let scene = moffat_field(w, h, &stars, 4.0, 0.0);
            let mut m: Vec<f32> = (0..w * h)
                .map(|i| {
                    let (gain, sky) = match site(BayerPattern::Rggb, i % w, i / w) {
                        'R' => (0.5, 900.0),
                        'B' => (0.4, 800.0),
                        _ => (1.0, 1000.0),
                    };
                    scene[i] * gain + sky
                })
                .collect();
            add_noise(&mut m, 5.0, seed + 1);
            let frame = RawFrame {
                planes: RawPlanes::Cfa { mosaic: m, pattern: BayerPattern::Rggb },
                w,
                h,
                domain: RAW,
                flip_vertical: false,
                channels: 1,
            };
            (frame, cat, wcs)
        }

        fn inputs(wcs: &WcsSolution) -> ZpInputs<'_> {
            ZpInputs { wcs, exptime_s: Some(T_EXP), epoch: 2025.5 }
        }

        /// The Analysis measurement's numbers, as the identity tests compare them.
        fn same_analysis(name: &str, pm: &PlaneMeasurement, plain: &PlaneMeasurement) {
            assert_eq!(format!("{:?}", pm.stars), format!("{:?}", plain.stars), "{name}: stars");
            assert_eq!(
                (
                    pm.beta.to_bits(),
                    pm.aperture_correction.map(f64::to_bits),
                    pm.reported_aperture_correction.map(f64::to_bits),
                    pm.aperture_stars,
                    pm.seeds
                ),
                (
                    plain.beta.to_bits(),
                    plain.aperture_correction.map(f64::to_bits),
                    plain.reported_aperture_correction.map(f64::to_bits),
                    plain.aperture_stars,
                    plain.seeds
                ),
                "{name}"
            );
            assert_eq!(format!("{:?}", pm.frame), format!("{:?}", plain.frame), "{name}: frame");
        }

        #[test]
        fn the_composition_measures_a_synthetic_frame() {
            let (frame, cat, wcs) = solved_mono(500, 2000, 1500, 31);
            let asked = std::cell::Cell::new(0);
            let (pm, run) = measure_with_zero_point(
                &frame,
                &cfg(),
                None,
                Some(&inputs(&wcs)),
                &ForcedListParams::default(),
                |_, w, h, frac, _| {
                    asked.set(asked.get() + 1);
                    assert_eq!((w, h, frac), (2000, 1500, 0.9));
                    Ok(cat.clone())
                },
            );
            assert_eq!(asked.get(), 1, "the cone is read once");
            let run = run.expect("a solved frame is judged");
            let z = run.result.unwrap_or_else(|r| panic!("refused: {r:?}; anchor {:?}", run.anchor));
            assert!((z.zero_point - ZP_TRUE).abs() < 0.02, "zero point {}", z.zero_point);
            assert!(z.stars >= 50, "survivors {}", z.stars);
            let anchor = run.anchor.expect("anchor");
            assert!((anchor.zero_point - ZP_TRUE).abs() < 0.05, "anchor {}", anchor.zero_point);
            let list = run.list.expect("list");
            assert!(
                list.stars.len() >= 50 && list.window_bright < 15.0,
                "{} stars, G_b {}",
                list.stars.len(),
                list.window_bright
            );
            let o = run.outcome.expect("outcome");
            let (map, gate) = (run.map.expect("the map"), run.gate.expect("the gate"));
            assert!(map.members >= 30 && map.residual_px < 0.2, "map {} members, residual {}", map.members, map.residual_px);
            assert!(gate.kept_share >= 0.9, "kept share {}", gate.kept_share);
            assert!(o.members >= 50 && o.pedestal.is_some(), "{} members, pedestal {:?}", o.members, o.pedestal);
            let lm = run.measured.expect("measured");
            assert_eq!(lm.apertures.len(), list.stars.len());
            assert_eq!(lm.fits.len(), list.map.len());
            assert!(pm.stars.len() >= 400);
        }

        /// L1: the one path where the budget reports an aperture correction: a Bayer mosaic. The map stars are fitted on the
        /// green sites and every list star is summed on them, the anchor is the budget fits' zero point times the reported `c` (c = 1.22 here: without it the anchor
        /// misses by 0.21 mag), and the zero point lands within 0.01 mag of the truth the catalogue was made from (0.002
        /// measured).
        #[test]
        fn a_mosaic_is_listed_on_its_green_sites_and_anchored_with_the_reported_c() {
            let (frame, cat, wcs) = solved_mosaic(500, 2000, 1500, 61);
            let params = ForcedListParams::default();
            let plain = measure_raw_frame(&frame, &cfg(), None);
            let (pm, run) =
                measure_with_zero_point(&frame, &cfg(), None, Some(&inputs(&wcs)), &params, |_, _, _, _, _| Ok(cat.clone()));
            same_analysis("mosaic with inputs", &pm, &plain);
            let run = run.expect("a solved frame is judged");

            // The budget reports c on a mosaic, and the anchor is the fits' zero point times it.
            let c = pm.reported_aperture_correction.expect("a mosaic reports its aperture correction");
            assert!(c.is_finite() && c > 1.1, "reported c {c}: the premise of the test is a c that matters");
            let anchor = run.anchor.expect("anchor");
            let bare = anchor_zero_point(
                &star_metrics(&pm),
                None,
                &wcs,
                &cat,
                Some(T_EXP),
                frame.w,
                frame.h,
                pm.frame.fwhm_px.unwrap_or(0.0),
                &params,
            )
            .expect("the bare anchor");
            assert!((anchor.zero_point - bare.zero_point - 2.5 * c.log10()).abs() < 1e-9, "the anchor takes c");
            assert!((anchor.zero_point - ZP_TRUE).abs() < 0.05, "anchor {} (truth {ZP_TRUE})", anchor.zero_point);
            assert!(
                (bare.zero_point - ZP_TRUE).abs() > 0.1,
                "without c the anchor misses by {}: the test sees whether c is applied",
                (bare.zero_point - ZP_TRUE).abs()
            );

            // The list: measured on the green sites, apertures for every list star, the map's second fit kept.
            let list = run.list.as_ref().expect("list");
            let lm = run.measured.as_ref().expect("the list was measured");
            assert!(lm.refusal.is_none() && lm.apertures.len() == list.stars.len() && list.stars.len() >= 50);
            assert!(lm.apertures.iter().filter(|a| a.on_frame).count() >= 50, "apertures on the frame");
            assert!(run.gate.is_some_and(|g| g.kept_share >= 0.9), "{:?}", run.gate);
            let z = run.result.unwrap_or_else(|r| panic!("refused: {r:?}; anchor {:?}", run.anchor));
            assert!((z.zero_point - ZP_TRUE).abs() < 0.02, "zero point {} (truth {ZP_TRUE})", z.zero_point);
            assert!(z.stars >= 50, "survivors {}", z.stars);
        }

        /// L2: with a solve the list is measured and the statistic is computed, and the Analysis numbers are still the
        /// plain measurement's, bit for bit.
        #[test]
        fn the_analysis_numbers_do_not_move_with_a_zero_point() {
            let (frame, cat, wcs) = solved_mono(300, 1400, 1000, 41);
            let plain = measure_raw_frame(&frame, &cfg(), None);
            let (with, run) = measure_with_zero_point(
                &frame,
                &cfg(),
                None,
                Some(&inputs(&wcs)),
                &ForcedListParams::default(),
                |_, _, _, _, _| Ok(cat.clone()),
            );
            let run = run.expect("a solved frame is judged");
            let lm = run.measured.as_ref().expect("a list was measured (the identity is not vacuous)");
            assert!(!lm.fits.is_empty() && lm.fits.iter().filter(|f| f.kept).count() >= 50, "{} fits", lm.fits.len());
            assert!(run.result.is_ok(), "the list gave a zero point: {:?}", run.result);
            let (without, none) = measure_with_zero_point(
                &frame,
                &cfg(),
                None,
                None,
                &ForcedListParams::default(),
                |_, _, _, _, _| panic!("no solve, no cone"),
            );
            assert!(none.is_none());
            same_analysis("with", &with, &plain);
            same_analysis("without", &without, &plain);
        }

        #[test]
        fn a_three_plane_frame_and_a_trailed_plane_give_their_refusal_codes() {
            let wcs = wcs_1as(600, 450);
            let no_cone = |_: &WcsSolution, _: usize, _: usize, _: f64, _: f64| -> Result<Vec<CatalogStar>> {
                panic!("the cone is not read for this frame")
            };
            // A debayered three-plane file.
            let (mono, _, _) = solved_mono(100, 600, 450, 3);
            let RawPlanes::Mono(p) = &mono.planes else { unreachable!() };
            let rgb = RawFrame {
                planes: RawPlanes::Rgb([p.clone(), p.clone(), p.clone()]),
                w: 600,
                h: 450,
                domain: RAW,
                flip_vertical: false,
                channels: 3,
            };
            let (_, run) =
                measure_with_zero_point(&rgb, &cfg(), None, Some(&inputs(&wcs)), &ForcedListParams::default(), no_cone);
            assert_eq!(run.unwrap().result, Err(ForcedRefusal::ColourCamera));
            // A trailed plane.
            let trailed: Vec<f32> =
                crate::test_support::trailed_field(600, 450, 9).iter().map(|v| v * 65535.0).collect();
            let frame = RawFrame {
                planes: RawPlanes::Mono(trailed.clone()),
                w: 600,
                h: 450,
                domain: RAW,
                flip_vertical: false,
                channels: 1,
            };
            let (pm, run) =
                measure_with_zero_point(&frame, &cfg(), None, Some(&inputs(&wcs)), &ForcedListParams::default(), no_cone);
            assert!(pm.trailed);
            let r = run.unwrap().result.unwrap_err();
            assert_eq!((r, r.code(), r.is_stored()), (ForcedRefusal::FrameVerdict, "frame_verdict", true));
        }

        /// L4: a colour file is `colour_camera` even when its plane is trailed: rustafits returns `None` for a trailed
        /// plane before it looks at the layout, so the composition checks the layout first.
        #[test]
        fn a_trailed_three_plane_file_is_still_a_colour_camera() {
            let wcs = wcs_1as(600, 450);
            let trailed: Vec<f32> =
                crate::test_support::trailed_field(600, 450, 9).iter().map(|v| v * 65535.0).collect();
            let rgb = RawFrame {
                planes: RawPlanes::Rgb([trailed.clone(), trailed.clone(), trailed]),
                w: 600,
                h: 450,
                domain: RAW,
                flip_vertical: false,
                channels: 3,
            };
            let (pm, run) = measure_with_zero_point(
                &rgb,
                &cfg(),
                None,
                Some(&inputs(&wcs)),
                &ForcedListParams::default(),
                |_, _, _, _, _| panic!("the cone is not read for a colour file"),
            );
            assert!(pm.trailed || pm.defocused, "premise: the plane has a verdict of its own");
            let r = run.unwrap().result.unwrap_err();
            assert_eq!((r, r.code(), r.is_stored()), (ForcedRefusal::ColourCamera, "colour_camera", true));
        }

        #[test]
        fn exposure_solve_and_catalogue_refusals_are_told_apart() {
            let (frame, cat, wcs) = solved_mono(300, 1400, 1000, 51);
            let judged = |zp: ZpInputs, cone: Result<Vec<CatalogStar>>| {
                let cone = std::cell::Cell::new(Some(cone));
                measure_with_zero_point(&frame, &cfg(), None, Some(&zp), &ForcedListParams::default(), |_, _, _, _, _| {
                    cone.take().unwrap()
                })
                .1
                .unwrap()
                .result
            };
            let none = ZpInputs { exptime_s: None, ..inputs(&wcs) };
            assert_eq!(judged(none, Ok(cat.clone())), Err(ForcedRefusal::NoExposure));
            let mut bad = wcs_1as(1400, 1000);
            bad.cd = [[0.0, 0.0], [0.0, 0.0]];
            assert_eq!(judged(inputs(&bad), Ok(cat.clone())), Err(ForcedRefusal::UnreadableSolve));
            let r = judged(inputs(&wcs), Err(anyhow::anyhow!("tile unreadable"))).unwrap_err();
            assert_eq!((r, r.is_stored()), (ForcedRefusal::CatalogueUnreadable, false));
            // An empty cone: no matches for the anchor.
            assert!(matches!(judged(inputs(&wcs), Ok(Vec::new())), Err(ForcedRefusal::NoAnchor { kept: 0 })));
        }

        /// M2: a panic inside the cone (the rustafits callback) must not take the Analysis measurement down with it: the
        /// call still returns the measurement, bit-identical, and a refusal that stores nothing.
        #[test]
        fn a_panicking_cone_keeps_the_measurement_and_stores_nothing() {
            let (frame, _, wcs) = solved_mono(300, 1400, 1000, 71);
            let plain = measure_raw_frame(&frame, &cfg(), None);
            for what in ["a str payload", "a String payload"] {
                let (pm, run) = measure_with_zero_point(
                    &frame,
                    &cfg(),
                    None,
                    Some(&inputs(&wcs)),
                    &ForcedListParams::default(),
                    |_, _, _, _, _| -> Result<Vec<CatalogStar>> {
                        if what.starts_with("a str") {
                            panic!("the tile reader panicked")
                        } else {
                            panic!("{}", "the tile reader panicked, formatted")
                        }
                    },
                );
                same_analysis(what, &pm, &plain);
                let run = run.expect("a solved frame is judged");
                let r = run.result.unwrap_err();
                assert_eq!((r, r.code(), r.is_stored()), (ForcedRefusal::CatalogueUnreadable, "catalogue_unreadable", false));
                assert!(run.list.is_none() && run.measured.is_none() && run.anchor.is_none());
            }
        }
        /// R4 / M5: a list whose window holds no map star carries the list's size in `too_few_stars`, not 0.
        #[test]
        fn an_empty_map_request_carries_the_list_size() {
            let (frame, cat, wcs) = solved_mono(300, 1400, 1000, 51);
            // Map stars are the list stars with G <= G_b + map_window_mag: a map window of -10 mag leaves none.
            let params = ForcedListParams { map_window_mag: -10.0, ..ForcedListParams::default() };
            let (_, run) =
                measure_with_zero_point(&frame, &cfg(), None, Some(&inputs(&wcs)), &params, |_, _, _, _, _| Ok(cat.clone()));
            let run = run.expect("a solved frame is judged");
            let list = run.list.as_ref().expect("the list was built");
            assert!(list.stars.len() >= 50 && list.map.is_empty());
            assert_eq!(run.result, Err(ForcedRefusal::TooFewStars { list: list.stars.len() as u32, kept: 0 }));
            assert!(run.measured.is_none());
        }

        /// A panic of the placement callback keeps the Analysis measurement and stores nothing.
        #[test]
        fn a_panicking_placement_keeps_the_measurement() {
            let (frame, cat, wcs) = solved_mono(300, 1400, 1000, 71);
            let plain = measure_raw_frame(&frame, &cfg(), None);
            PLACEMENT_PANICS.with(|c| c.set(true));
            let (pm, run) = measure_with_zero_point(
                &frame,
                &cfg(),
                None,
                Some(&inputs(&wcs)),
                &ForcedListParams::default(),
                |_, _, _, _, _| Ok(cat.clone()),
            );
            PLACEMENT_PANICS.with(|c| c.set(false));
            same_analysis("placement panic", &pm, &plain);
            let r = run.expect("a solved frame is judged").result.unwrap_err();
            assert_eq!((r, r.code(), r.is_stored()), (ForcedRefusal::Internal, "internal", false));
        }

        #[test]
        fn a_panicking_statistic_keeps_the_measurement() {
            let (frame, cat, wcs) = solved_mono(300, 1400, 1000, 71);
            let plain = measure_raw_frame(&frame, &cfg(), None);
            STATISTIC_PANICS.with(|c| c.set(true));
            let (pm, run) = measure_with_zero_point(
                &frame,
                &cfg(),
                None,
                Some(&inputs(&wcs)),
                &ForcedListParams::default(),
                |_, _, _, _, _| Ok(cat.clone()),
            );
            STATISTIC_PANICS.with(|c| c.set(false));
            same_analysis("statistic panic", &pm, &plain);
            let run = run.expect("a solved frame is judged");
            let r = run.result.unwrap_err();
            assert_eq!((r, r.code(), r.is_stored()), (ForcedRefusal::Internal, "internal", false));
            // The figures the gate had not produced are absent; the map and the placement stand.
            assert!(run.gate.is_none() && run.outcome.is_none() && run.map.is_some() && run.placement.is_some());
        }
    }
}

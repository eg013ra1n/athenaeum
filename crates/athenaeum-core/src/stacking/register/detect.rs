//! Registration stars (spec §3.1): the detector with Moffat centroid
//! refinement on a luminance plane, then the saturation / eccentricity /
//! SNR cuts and the brightest-`maxStars` truncation.

use std::borrow::Cow;
use std::sync::Arc;

use astroimage::ImageAnalyzer;

use super::DetectionConfig;
use crate::stacking::measure::ADU_SCALE;
use crate::stacking::psf_signal::StarFit;

/// Absolute saturation level in native `[0, 1]` units: a star whose
/// `peak + background` reaches it has a flat top and no usable centroid.
pub const SATURATION: f32 = 0.95;

/// Calibrated frames are in [0, 1]; a plane whose finite maximum exceeds
/// this was never scaled down (a float32 source keeps ADU) and every star
/// would fail the saturation cut after the `ADU_SCALE` multiply.
pub const NATIVE_UNITS_MAX: f32 = 1.5;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Star {
    pub x: f64,
    pub y: f64,
    pub flux: f64,
    /// Fitted centroid σ per axis (px) when the detector's refinement
    /// succeeded; `None` keeps the pass-1 centroid unweighted.
    pub sigma: Option<(f64, f64)>,
}

/// `0.25 R + 0.5 G + 0.25 B` for three planes, the plane itself for one,
/// the plain mean otherwise. Planes are row-major `width × height`.
///
/// A single plane needs no combination at all (perf tier A Task 3): the
/// mono case borrows the caller's slice (`Cow::Borrowed`) instead of
/// copying it into a fresh `Vec` via `to_vec()`. [`detect_stars`] — the
/// only real reader of the result — only ever reads `lum`, never mutates
/// or takes ownership of it (it builds its own ADU-scaled buffer instead),
/// so the borrow is sound for as long as the caller keeps its own plane
/// data alive; `register/frame.rs`'s `reference_stars`/`detect_frame_stars`
/// keep the read planes in scope across both this call and the
/// `detect_stars` call that follows it for exactly that reason.
pub fn luminance<'a>(planes: &[&'a [f32]]) -> Cow<'a, [f32]> {
    match planes {
        [p] => Cow::Borrowed(*p),
        [r, g, b] => Cow::Owned(
            r.iter()
                .zip(g.iter())
                .zip(b.iter())
                .map(|((r, g), b)| 0.25 * r + 0.5 * g + 0.25 * b)
                .collect(),
        ),
        [] => Cow::Owned(Vec::new()),
        _ => {
            let k = 1.0 / planes.len() as f32;
            let mut acc = vec![0.0f32; planes[0].len()];
            for p in planes {
                for (a, v) in acc.iter_mut().zip(p.iter()) {
                    *a += k * v;
                }
            }
            Cow::Owned(acc)
        }
    }
}

/// Detect registration stars on one luminance plane in native units. The
/// detector runs on an ADU-scaled copy (its thresholds are tuned for 16-bit
/// data); results come back in pixel coordinates and native flux.
pub fn detect_stars(
    lum: &[f32],
    w: usize,
    h: usize,
    cfg: &DetectionConfig,
    max_stars: usize,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> Vec<Star> {
    if w < 8 || h < 8 || lum.len() < w * h {
        return Vec::new();
    }
    // Fused (perf tier A Task 3): the max-fold and the ADU scale used to be
    // two separate passes over `lum`. `max` accumulates as a side effect of
    // the SAME map that builds `scaled` — same starting value (0.0), same
    // `f32::max` reduction, same finite-only filter (a non-finite `v` skips
    // the fold update but still gets scaled into `scaled`, exactly as the
    // two-pass version scaled every element regardless of the fold's own
    // filter) — so `max` is bit-identical to the old
    // `lum.iter().copied().filter(f32::is_finite).fold(0.0, f32::max)`, and
    // `scaled` is bit-identical to the old `lum.iter().map(|v| v *
    // ADU_SCALE).collect()`. `lum` is now the ONLY buffer this function
    // reads and `scaled` the only one it allocates — `luminance`'s own
    // single-plane case no longer allocates a second copy upstream (see its
    // doc comment).
    let mut max = 0.0f32;
    let scaled: Vec<f32> = lum
        .iter()
        .map(|&v| {
            if v.is_finite() {
                max = f32::max(max, v);
            }
            v * ADU_SCALE
        })
        .collect();
    if max > NATIVE_UNITS_MAX {
        tracing::warn!(
            max,
            "plane exceeds native units; the saturation cut will drop every star"
        );
    }
    let mut analyzer = ImageAnalyzer::new()
        .with_max_stars((max_stars + max_stars / 4).max(8))
        .with_centroid_refine(true);
    if let Some(p) = pool {
        analyzer = analyzer.with_thread_pool(Arc::clone(p));
    }
    let result = match analyzer.detect_fast_data(&scaled, w, h, 1) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "star detection failed; no registration stars");
            return Vec::new();
        }
    };
    let saturation = SATURATION * ADU_SCALE;
    let mut stars: Vec<Star> = result
        .stars
        .iter()
        .filter(|s| s.peak + result.background < saturation)
        .filter(|s| s.eccentricity <= cfg.max_eccentricity)
        .filter(|s| s.snr >= cfg.min_snr)
        .map(|s| Star {
            x: s.x as f64,
            y: s.y as f64,
            flux: (s.flux / ADU_SCALE) as f64,
            sigma: (s.sx > 0.0 && s.sy > 0.0).then(|| (s.sx as f64, s.sy as f64)),
        })
        .collect();
    stars.sort_by(|a, b| b.flux.total_cmp(&a.flux));
    stars.truncate(max_stars);
    stars
}

/// This detector's own saturation / eccentricity / SNR cuts (the filter
/// chain [`detect_stars`] applies above), applied instead to an already
/// completed PSF fit (Tier C Task 2, spec §2.2.3) — originally shared by
/// Task 2 (LN maps Measure's fits through the registration instead of
/// re-detecting on the warped frame) and the still-pending Task 3
/// (Register reuses Measure's fits for mono frames). Measure's own
/// acceptance (`psf_signal::accept`: centroid tolerance, residual cap,
/// region containment) does not reject a saturated or elongated star the
/// way this module always has, so a caller that wants to treat a
/// `StarFit` list as if it had come through this detector needs this rule
/// applied explicitly.
///
/// **Fix round 2, ruling C-12**: Task 2's own LN path no longer calls this
/// — the diagnostics round (ruling C-11) found a Moffat fit's `signal` is
/// not warp-invariant (a fit on the native frame and a fit of the same
/// star on the warped frame integrate measurably different flux, worse
/// for undersampled stars), so LN now uses Measure's fits as SEED
/// POSITIONS ONLY and re-fits on the warped plane
/// (`stacking::ln::scale::relative_scale_from_seeds`), guarding against a
/// saturated WARPED pixel directly instead of re-checking the (irrelevant,
/// now-discarded) native flux this function was built to validate. Kept,
/// with its own tests intact, for Task 3's still-pending reuse — a mono
/// Register fit genuinely wants this exact rule.
///
/// `fit`'s `background`/`amplitude` are assumed `measure::ADU_SCALE`-scaled
/// — [`measure::measure_plane_with_seeds`] fits at exactly that scale
/// (the same convention `saturation` below already uses), so a `StarFit`
/// read back from a `fits` artifact needs no further conversion for this
/// check alone.
///
/// [`StarFit`] carries no raw detector SNR (there was no detection here —
/// the fit already exists), so this uses `1 / residual` as a proxy:
/// [`StarFit::residual`] is `sqrt(cost/n) / amplitude`, the fit's own RMS
/// residual as a fraction of its amplitude, so its reciprocal reads as an
/// amplitude-to-noise ratio in the same spirit as a detector SNR — not
/// numerically identical to `astroimage`'s own per-star SNR, but it cuts
/// the same population: a fit whose noise is a large fraction of its own
/// signal.
pub(crate) fn passes_register_cuts(fit: &StarFit, cfg: &DetectionConfig) -> bool {
    let saturation = (SATURATION * ADU_SCALE) as f64;
    if fit.background + fit.amplitude >= saturation {
        return false;
    }
    if fit.eccentricity() > cfg.max_eccentricity as f64 {
        return false;
    }
    let snr = if fit.residual > 0.0 {
        1.0 / fit.residual
    } else {
        f64::INFINITY
    };
    if snr < cfg.min_snr as f64 {
        return false;
    }
    true
}

/// Gaussian FWHM -> sigma, `2 * sqrt(2 * ln 2)` — the same conversion
/// rustafits' own fast detector uses for `FastStar::sx`/`sy` (`sigma_x =
/// FWHM_x / 2.3548`), so a fit-derived star's [`Star::sigma`] carries the
/// same per-axis meaning a freshly detected one would, for `align.rs`'s
/// weighted pairing (`pair_projected`) to use identically either way.
const FWHM_TO_SIGMA: f64 = 2.354_820_045_030_949_3;

/// Turn one plane's accepted PSF fits (Tier C Task 3, spec §7, ruling C-6)
/// into a registration star list, in the same shape [`detect_stars`]
/// itself returns: [`passes_register_cuts`] applied per fit (this
/// detector's own saturation/eccentricity/SNR cuts — Measure's own
/// acceptance does not reject those the way this module always has),
/// sorted brightest-first, truncated to `max_stars`. Mono frames only — a
/// single-plane frame's luminance IS the measured plane, so the fitted
/// centroid already IS the plane's own detection; an OSC (multi-plane)
/// frame has no single luminance fit to reuse this way (the caller decides
/// mono vs OSC — see [`crate::stacking::register::frame::detect_frame_stars`]).
///
/// Fix round 1 (ruling C-18, item 2): [`Star::flux`] is `fit.signal /
/// ADU_SCALE`, NOT the raw `fit.signal` — [`StarFit`]'s fields are
/// ADU-scaled (`measure::measure_plane_with_seeds` fits at exactly that
/// scale, the same convention [`passes_register_cuts`]'s own saturation
/// check above already uses), while [`detect_stars`] stores `s.flux /
/// ADU_SCALE`, the NATIVE `[0, 1]`-ish domain. A fit-derived star that
/// skipped the division would carry a flux ~65535× too large — silently
/// wrong for any caller that reads `Star.flux` absolutely rather than as a
/// same-list ranking key (`ln::scale::to_seed` does exactly that).
pub(crate) fn stars_from_fits(
    fits: &[StarFit],
    cfg: &DetectionConfig,
    max_stars: usize,
) -> Vec<Star> {
    let mut stars: Vec<Star> = fits
        .iter()
        .filter(|fit| passes_register_cuts(fit, cfg))
        .map(|fit| {
            let (sx, sy) = (fit.fwhm_x / FWHM_TO_SIGMA, fit.fwhm_y / FWHM_TO_SIGMA);
            Star {
                x: fit.x,
                y: fit.y,
                flux: fit.signal / ADU_SCALE as f64,
                sigma: (sx > 0.0 && sy > 0.0).then_some((sx, sy)),
            }
        })
        .collect();
    stars.sort_by(|a, b| b.flux.total_cmp(&a.flux));
    stars.truncate(max_stars);
    stars
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{add_noise, gaussian_field, moffat_field, MoffatStar};

    fn near(stars: &[Star], x: f64, y: f64, r: f64) -> Option<&Star> {
        stars
            .iter()
            .find(|s| (s.x - x).abs() <= r && (s.y - y).abs() <= r)
    }

    #[test]
    fn luminance_weights_and_passthrough() {
        let r = [1.0f32, 0.0, 0.0];
        let g = [0.0f32, 1.0, 0.0];
        let b = [0.0f32, 0.0, 1.0];
        let rgb = luminance(&[&r, &g, &b]);
        assert_eq!(&*rgb, [0.25, 0.5, 0.25].as_slice());
        assert!(
            matches!(rgb, Cow::Owned(_)),
            "the 3-plane combination must allocate"
        );
        let mono = luminance(&[&r]);
        assert_eq!(&*mono, r.as_slice());
        assert!(
            matches!(mono, Cow::Borrowed(_)),
            "a single plane must not allocate"
        );
    }

    /// Perf tier A Task 3: the single-plane case must not copy — the
    /// returned slice's data pointer is the ORIGINAL plane's, not a fresh
    /// allocation with equal (but distinct) contents.
    #[test]
    fn luminance_borrows_the_single_plane_zero_copy() {
        let p = [1.0f32, 2.0, 3.0, 4.0];
        let lum = luminance(&[&p]);
        assert_eq!(lum.as_ref().as_ptr(), p.as_ptr());
    }

    #[test]
    fn cuts_saturated_elongated_and_faint_stars_and_keeps_the_brightest() {
        let (w, h) = (300, 300);
        // 40 ordinary stars on a grid, plus the three that must be cut.
        let mut stars: Vec<(f64, f64, f64)> = Vec::new();
        for j in 0..5 {
            for i in 0..8 {
                stars.push((
                    30.0 + i as f64 * 34.0,
                    40.0 + j as f64 * 50.0,
                    0.05 + 0.01 * (i + j) as f64,
                ));
            }
        }
        stars.push((150.0, 150.0, 0.95)); // saturated: peak + background = 1.0
        stars.push((60.0, 280.0, 0.003)); // faint: peak ≈ 1.5 σ
        let mut data = gaussian_field(w, h, &stars, 1.8, 0.05);
        let streak = moffat_field(
            w,
            h,
            &[MoffatStar {
                x: 240.0,
                y: 280.0,
                amp: 0.3,
                alpha_x: 8.0,
                alpha_y: 2.0,
                theta: 0.3,
            }],
            4.0,
            0.05,
        );
        for (d, s) in data.iter_mut().zip(streak) {
            *d += s - 0.05;
        }
        add_noise(&mut data, 0.002, 5);
        let cfg = DetectionConfig::default();
        let found = detect_stars(&data, w, h, &cfg, 2000, None);
        assert!(found.len() >= 36, "found {}", found.len());
        assert!(
            near(&found, 30.0, 40.0, 0.3).is_some(),
            "an ordinary star is missing"
        );
        assert!(
            near(&found, 150.0, 150.0, 3.0).is_none(),
            "saturated star kept"
        );
        assert!(near(&found, 60.0, 280.0, 3.0).is_none(), "faint star kept");
        assert!(
            near(&found, 240.0, 280.0, 4.0).is_none(),
            "elongated star kept"
        );
        assert!(
            found.windows(2).all(|p| p[0].flux >= p[1].flux),
            "not sorted by flux"
        );
        assert!(
            found.iter().filter(|s| s.sigma.is_some()).count() >= found.len() / 2,
            "refined σ missing on most stars"
        );
        // A smaller cap changes the detector's threshold and therefore every
        // star's aperture flux by ~1 %, so compare the truncated list by star
        // identity, not by flux.
        let top = detect_stars(&data, w, h, &cfg, 10, None);
        assert_eq!(top.len(), 10);
        for t in &top {
            assert!(
                found[..12]
                    .iter()
                    .any(|f| (f.x - t.x).abs() < 0.5 && (f.y - t.y).abs() < 0.5),
                "top-10 star ({}, {}) is not among the 12 brightest of the full run",
                t.x,
                t.y
            );
        }
    }

    #[test]
    fn empty_and_flat_planes_yield_no_stars() {
        assert!(detect_stars(&[], 0, 0, &DetectionConfig::default(), 100, None).is_empty());
        assert!(detect_stars(
            &vec![0.1f32; 64 * 64],
            64,
            64,
            &DetectionConfig::default(),
            100,
            None
        )
        .is_empty());
    }

    /// Perf tier A Task 3: `detect_stars` on a one-plane luminance must
    /// keep returning exactly the same `Vec<Star>` after the `luminance`
    /// borrow + fused max/ADU-scale pass. The expected values were captured
    /// from this SAME field/seed run through the pre-refactor implementation
    /// (`git show df219c49:crates/athenaeum-core/src/stacking/register/
    /// detect.rs`, before this task's own commit) — see the Task 3 report
    /// for the exact command. A change to detection, the ADU scale, or the
    /// max-fold semantics would move these numbers.
    #[test]
    fn detect_stars_on_one_plane_matches_the_pre_refactor_fixture() {
        let (w, h) = (128, 96);
        let stars = vec![
            (24.0, 30.0, 0.22),
            (90.0, 20.0, 0.16),
            (60.0, 70.0, 0.30),
            (105.0, 80.0, 0.11),
            (15.0, 60.0, 0.08),
        ];
        let mut data = gaussian_field(w, h, &stars, 1.7, 0.04);
        add_noise(&mut data, 0.0015, 13);
        let cfg = DetectionConfig::default();
        let found = detect_stars(&data, w, h, &cfg, 50, None);
        let expected = [
            Star {
                x: 59.99551773071289,
                y: 69.99957275390625,
                flux: 5.074063301086426,
                sigma: Some((1.530297875404358, 1.5253788232803345)),
            },
            Star {
                x: 23.993528366088867,
                y: 29.989356994628906,
                flux: 3.9069221019744873,
                sigma: Some((1.544712781906128, 1.5313700437545776)),
            },
            Star {
                x: 90.0020523071289,
                y: 20.00041961669922,
                flux: 2.679633378982544,
                sigma: Some((1.528847098350525, 1.5462924242019653)),
            },
            Star {
                x: 105.00074005126953,
                y: 79.99053192138672,
                flux: 1.8644146919250488,
                sigma: Some((1.5202736854553223, 1.523754358291626)),
            },
            Star {
                x: 14.986310958862305,
                y: 60.00912094116211,
                flux: 1.3920038938522339,
                sigma: Some((1.5292351245880127, 1.5237141847610474)),
            },
        ];
        assert_eq!(found.len(), expected.len());
        for (f, e) in found.iter().zip(expected.iter()) {
            assert_eq!(f, e, "detection drifted from the pre-refactor fixture");
        }
    }

    /// Collect `(level, message)` of every event emitted on THIS thread
    /// inside `f`. Same scoped-`with_default` + custom-`Layer` pattern
    /// `api::masters`'s and `logging::config`'s tests use, including the
    /// interest-cache rebuild (macro callsites cache their verdict per
    /// process, so a callsite first hit with no subscriber would otherwise
    /// stay disabled).
    fn capture_events<T>(f: impl FnOnce() -> T) -> (T, Vec<(String, String)>) {
        use std::sync::{Arc, Mutex};
        use tracing_subscriber::layer::SubscriberExt;

        #[derive(Clone, Default)]
        struct Seen(Arc<Mutex<Vec<(String, String)>>>);
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
        }
        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Seen {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _ctx: tracing_subscriber::layer::Context<'_, S>,
            ) {
                let mut msg = Message(String::new());
                event.record(&mut msg);
                self.0
                    .lock()
                    .unwrap()
                    .push((event.metadata().level().to_string(), msg.0));
            }
        }

        let seen = Seen::default();
        let subscriber = tracing_subscriber::registry().with(seen.clone());
        let out = tracing::subscriber::with_default(subscriber, || {
            tracing::callsite::rebuild_interest_cache();
            f()
        });
        let events = seen.0.lock().unwrap().clone();
        (out, events)
    }

    /// Perf tier A Task 3: the max-fold, now fused into the ADU-scale pass,
    /// must still fire the exact same warning under the exact same
    /// condition — a plane whose finite maximum exceeds `NATIVE_UNITS_MAX`.
    #[test]
    fn warns_when_a_plane_exceeds_native_units() {
        let cfg = DetectionConfig::default();
        let over = vec![2.0f32; 64 * 64];
        let (_, events) = capture_events(|| detect_stars(&over, 64, 64, &cfg, 10, None));
        assert!(
            events.iter().any(|(level, msg)| level == "WARN"
                && msg == "plane exceeds native units; the saturation cut will drop every star"),
            "expected the native-units warning, got {events:?}"
        );

        // At or under the threshold: no warning. NATIVE_UNITS_MAX is 1.5.
        let under = vec![1.0f32; 64 * 64];
        let (_, events) = capture_events(|| detect_stars(&under, 64, 64, &cfg, 10, None));
        assert!(
            !events
                .iter()
                .any(|(_, msg)| msg.contains("plane exceeds native units")),
            "did not expect the native-units warning, got {events:?}"
        );

        // A non-finite value must not itself trip the warning — it is
        // excluded from the max the same way the two-pass version excluded
        // it via `.filter(|v| v.is_finite())`.
        let mut with_nan = vec![0.5f32; 64 * 64];
        with_nan[10] = f32::NAN;
        with_nan[20] = f32::INFINITY;
        let (_, events) = capture_events(|| detect_stars(&with_nan, 64, 64, &cfg, 10, None));
        assert!(
            !events
                .iter()
                .any(|(_, msg)| msg.contains("plane exceeds native units")),
            "non-finite values must not trip the warning, got {events:?}"
        );
    }

    /// A [`StarFit`] with every field defaulted to a "good" value — the
    /// tests below flip exactly one field per case.
    fn good_fit() -> StarFit {
        StarFit {
            x: 100.0,
            y: 100.0,
            background: 100.0,
            amplitude: 5000.0,
            fwhm_x: 3.0,
            fwhm_y: 3.0,
            fwtm_x: 6.0,
            fwtm_y: 6.0,
            theta: 0.0,
            beta: 4.0,
            residual: 0.02,
            signal: 40000.0,
            area: 28.0,
        }
    }

    #[test]
    fn passes_register_cuts_keeps_a_good_fit() {
        assert!(passes_register_cuts(
            &good_fit(),
            &DetectionConfig::default()
        ));
    }

    #[test]
    fn passes_register_cuts_drops_a_saturated_fit() {
        let saturated = StarFit {
            // background + amplitude just over `SATURATION * ADU_SCALE`.
            background: 1000.0,
            amplitude: (SATURATION * ADU_SCALE) as f64,
            ..good_fit()
        };
        assert!(!passes_register_cuts(
            &saturated,
            &DetectionConfig::default()
        ));
    }

    #[test]
    fn passes_register_cuts_drops_an_elongated_fit() {
        let elongated = StarFit {
            fwhm_x: 10.0,
            fwhm_y: 1.0,
            ..good_fit()
        };
        assert!(elongated.eccentricity() > DetectionConfig::default().max_eccentricity as f64);
        assert!(!passes_register_cuts(
            &elongated,
            &DetectionConfig::default()
        ));
    }

    #[test]
    fn passes_register_cuts_drops_a_low_snr_fit() {
        // `1 / residual` proxy below `min_snr` (10.0 by default): a
        // residual of 0.5 gives an "SNR" of 2.
        let noisy = StarFit {
            residual: 0.5,
            ..good_fit()
        };
        assert!(!passes_register_cuts(&noisy, &DetectionConfig::default()));
    }

    #[test]
    fn stars_from_fits_converts_position_flux_and_sigma() {
        let fit = good_fit();
        let stars = stars_from_fits(&[fit], &DetectionConfig::default(), 10);
        assert_eq!(stars.len(), 1);
        let s = &stars[0];
        assert_eq!((s.x, s.y), (fit.x, fit.y));
        // Ruling C-18, item 2: native domain (`fit.signal / ADU_SCALE`),
        // the SAME domain `detect_stars` stores (`s.flux / ADU_SCALE`) —
        // not the raw ADU-scaled `fit.signal`, which `ln::scale::to_seed`
        // (and any other caller reading `Star.flux` absolutely, not just
        // as a same-list ranking key) would otherwise read ~65535x too
        // large.
        assert_eq!(s.flux, fit.signal / ADU_SCALE as f64);
        assert!(
            s.flux < 2.0,
            "a fit-derived flux must be native-domain like a detected one, not ADU-scaled: {}",
            s.flux
        );
        let (sx, sy) = s.sigma.expect("a well-formed fwhm carries a sigma");
        assert!((sx - fit.fwhm_x / FWHM_TO_SIGMA).abs() < 1e-12);
        assert!((sy - fit.fwhm_y / FWHM_TO_SIGMA).abs() < 1e-12);
    }

    #[test]
    fn stars_from_fits_drops_what_passes_register_cuts_would_drop() {
        let saturated = StarFit {
            background: 1000.0,
            amplitude: (SATURATION * ADU_SCALE) as f64,
            ..good_fit()
        };
        let elongated = StarFit {
            fwhm_x: 10.0,
            fwhm_y: 1.0,
            ..good_fit()
        };
        let stars = stars_from_fits(
            &[good_fit(), saturated, elongated],
            &DetectionConfig::default(),
            10,
        );
        assert_eq!(stars.len(), 1, "only the good fit must survive: {stars:?}");
    }

    #[test]
    fn stars_from_fits_sorts_brightest_first_and_truncates() {
        let dim = StarFit {
            signal: 10.0,
            ..good_fit()
        };
        let bright = StarFit {
            signal: 90.0,
            ..good_fit()
        };
        let mid = StarFit {
            signal: 50.0,
            ..good_fit()
        };
        let stars = stars_from_fits(&[dim, bright, mid], &DetectionConfig::default(), 2);
        assert_eq!(stars.len(), 2);
        // Native domain (ruling C-18, item 2) — divided by `ADU_SCALE`,
        // but the ORDER (and which two of the three survive truncation)
        // is unaffected by that positive monotonic rescale.
        assert_eq!(stars[0].flux, 90.0 / ADU_SCALE as f64);
        assert_eq!(stars[1].flux, 50.0 / ADU_SCALE as f64);
    }
}

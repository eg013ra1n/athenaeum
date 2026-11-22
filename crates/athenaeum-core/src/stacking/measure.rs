//! Per-frame, per-channel measurement of calibrated frames (spec §4.1): the kernel (`starloom`) measures; this module
//! keeps what reads Athenaeum's header convention. The calibrated frame's gain is `EGAIN x ATH_CSCL`
//! ([`gain_from_header`]), and the four entry points below pass it to the kernel's gain-taking entries, so every
//! `measure_frame*` caller keeps its signature and its numbers.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use tracing::{debug, warn};

use super::psf_signal::StarFit;
// The two gain-taking kernel entries, imported privately: the wrappers below are the only way in.
use starloom::stacking::measure::{measure_frame_full, measure_frame_pass1_gain};
use crate::integration::IntegrationError;

// The kernel's names, listed one by one: `measure_frame_full` and `measure_frame_pass1_gain` take the gain as a
// parameter and stay out, so no entry point without Athenaeum's gain resolves under this path. The four `measure_frame*`
// wrappers below carry it. The SL-17 test hooks sit on their own line.
pub use starloom::stacking::measure::{
    channel_c_inputs, channel_c_summary, check_pass1_head, measure_frame_pass2, measure_plane, measure_plane_detailed,
    measure_plane_diagnosed, measure_plane_with_seeds, measure_planes_diagnosed, measure_planes_pass1,
    measure_planes_pass2, measure_planes_with_width_hints, substitute_group_c, ChannelCSummary, ChannelMeasurement,
    FrameMeasurement, FramePass1, MeasureOptions, MeasureTimings, MeasuredPlane, NoiseSource, PlaneMeasurement,
    PlanesPass1, SeedOverride, StarPopulation, ADU_SCALE, COLOUR_LIST_STARS, DEFAULT_DETECTION_SIGMA,
    DEFAULT_SEED_PREFILTER, MONO_LIST_PER_REGISTRATION_STAR,
};
#[cfg(feature = "test-support")]
pub use starloom::stacking::measure::{pass1_peak, PASS1_CANCEL_AFTER, PASS1_LOG, PASS1_TOUCH_AFTER};

/// The calibrated frame's gain in e- per CALIBRATED unit, read from its header (Plan A Task 11, ruling R11-6, PA-22):
/// `EGAIN` (e- per ADU of the raw sensor) times `ATH_CSCL` (the calibrated light's unit divisor, ADU per unit). `ATH_CFNM`
/// is the flat's normalisation and plays no part. No camera table: a frame without either card, or one that is not a
/// positive number, has no gain (`None`: the SNR omits its shot term and the frame line says so); an unreadable header
/// is logged, never silent.
pub fn gain_from_header(path: &Path) -> Option<f64> {
    let is_xisf = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("xisf"));
    let (egain, cscl) = if is_xisf {
        match crate::fits_parser::extract_xisf_header(path) {
            Ok(text) => (
                header_text_f64(&text, "EGAIN"),
                header_text_f64(&text, "ATH_CSCL"),
            ),
            Err(e) => {
                warn!(path = %path.display(), error = %e, "snr: the XISF header could not be read; no gain");
                return None;
            }
        }
    } else {
        match crate::fits_parser::FitsHeader::from_path(path) {
            Ok(h) => (h.get_f64("EGAIN"), h.get_f64("ATH_CSCL")),
            Err(e) => {
                warn!(path = %path.display(), error = %e, "snr: the FITS header could not be read; no gain");
                return None;
            }
        }
    };
    let gain = egain.zip(cscl).map(|(e, c)| e * c);
    if gain.is_none() {
        debug!(path = %path.display(), "snr: no EGAIN and ATH_CSCL in the header; the shot term is omitted");
    }
    gain.filter(|g| g.is_finite() && *g > 0.0)
}

/// The real value of `key` in the header text `extract_xisf_header` returns (`NAME = value` lines).
fn header_text_f64(text: &str, key: &str) -> Option<f64> {
    text.lines().find_map(|line| {
        let (k, v) = line.split_once('=')?;
        if k.trim() != key {
            return None;
        }
        v.split('/')
            .next()?
            .trim()
            .trim_matches('\'')
            .trim()
            .parse::<f64>()
            .ok()
    })
}

/// Measure every plane of a calibrated frame; checks `cancel` before each
/// plane.
pub fn measure_frame(
    path: &Path,
    opts: &MeasureOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
    cancel: &AtomicBool,
) -> Result<FrameMeasurement, IntegrationError> {
    measure_frame_with_seeds(path, opts, pool, cancel, SeedOverride::Fast)
}

/// As `measure_frame`, but every plane is seeded via `measure_plane_with_seeds`.
pub fn measure_frame_with_seeds(
    path: &Path,
    opts: &MeasureOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
    cancel: &AtomicBool,
    seed_source: SeedOverride,
) -> Result<FrameMeasurement, IntegrationError> {
    measure_frame_full(path, opts, pool, cancel, seed_source, gain_from_header(path)).map(|(m, _)| m)
}

/// As `measure_frame`, but also returns every plane's accepted [`StarFit`]s
/// (Tier C Task 1), indexed like `FrameMeasurement::channels` — `fits[i]`
/// is what fed `channels[i]`. The one caller that needs them today is
/// Measure's own stage 3 (`stacking::run`), which persists each plane's
/// list as the on-disk `fits` artifact beside `metrics`; every other
/// `measure_frame`/`measure_frame_with_seeds` caller is untouched by this
/// function's existence.
pub fn measure_frame_with_fits(
    path: &Path,
    opts: &MeasureOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
    cancel: &AtomicBool,
) -> Result<(FrameMeasurement, Vec<Vec<StarFit>>), IntegrationError> {
    measure_frame_full(path, opts, pool, cancel, SeedOverride::Fast, gain_from_header(path))
}

/// [`measure_frame_with_fits`]'s first half for the group measure (Task 11b): every plane's verdict pass and own β.
pub fn measure_frame_pass1(
    path: &Path,
    opts: &MeasureOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
    cancel: &AtomicBool,
) -> Result<FramePass1, IntegrationError> {
    measure_frame_pass1_gain(path, opts, pool, cancel, gain_from_header(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::write_fits_f32;
    use crate::star_measure as sm;

    /// Task 7 guard: the shot term of every star's SNR is the one the header's gain gives. The frame-level result drops
    /// the planes' `snr_gain`, so it is read back from the fits: a fit's `snr` is `star_snr` of the plane with the gain
    /// the measurement ran with. Core reads `EGAIN x ATH_CSCL` from the header (65535 ADU per unit, 50000 e- per unit);
    /// starloom's own `measure_frame` carries no gain (`None`).
    #[test]
    fn core_measure_frame_reads_the_gain() {
        use crate::fits_writer::{Card, CardValue};
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("gain.fits");
        let (w, h) = (64usize, 64usize);
        let stars = [
            (14.3, 15.2, 0.25),
            (45.6, 16.1, 0.2),
            (16.4, 46.7, 0.3),
            (47.2, 47.5, 0.22),
            (31.1, 31.4, 0.28),
        ];
        let data = crate::test_support::gaussian_field(w, h, &stars, 1.8, 0.08);
        let cards = [
            Card::new("EGAIN", CardValue::Real(50000.0 / 65535.0)).unwrap(),
            Card::new("ATH_CSCL", CardValue::Real(65535.0)).unwrap(),
        ];
        write_fits_f32(&p, w, h, 1, &data, &cards).unwrap();
        let opts = MeasureOptions::default();
        // Core's wrapper and the kernel take the same option type (`MeasureOptions` is starloom's); `kernel_opts` is the
        // kernel call's own default value.
        let kernel_opts = starloom::stacking::measure::MeasureOptions::default();
        let cancel = AtomicBool::new(false);
        let working: Vec<f32> = data.iter().map(|v| v * ADU_SCALE).collect();
        let want = Some(50000.0 / ADU_SCALE as f64);

        // A fit's `snr` is `star_snr` of the plane with the gain the measurement ran with; without a gain the module
        // gives no SNR at all (NaN).
        let snr_with = |fits: &[StarFit], g: Option<f64>| {
            fits.iter().all(|f| {
                sm::star_snr(&working, w, h, f, g, None, &sm::ApertureNoise::WHITE).0 == f.snr
            })
        };

        let (_, core_fits) = crate::stacking::measure::measure_frame_with_fits(&p, &opts, None, &cancel).unwrap();
        let (_, kernel_fits) =
            starloom::stacking::measure::measure_frame_with_fits(&p, &kernel_opts, None, &cancel).unwrap();
        assert!(
            core_fits[0].iter().filter(|f| f.snr.is_finite()).count() >= 3,
            "the fixture must give the guard stars to read: {} fits",
            core_fits[0].len()
        );
        assert!(
            snr_with(&core_fits[0], want),
            "core's measure_frame_with_fits runs the SNR with EGAIN x ATH_CSCL over ADU_SCALE"
        );
        assert!(
            kernel_fits[0].len() >= 3 && kernel_fits[0].iter().all(|f| f.snr.is_nan()),
            "starloom's measure_frame_with_fits carries no gain, so no SNR"
        );
        // `measure_frame` itself: the same frame measured through both entries; the SNR does not reach the channel, so
        // the two agree on everything the channel carries.
        let core_m = crate::stacking::measure::measure_frame(&p, &opts, None, &cancel).unwrap();
        let kernel_m = starloom::stacking::measure::measure_frame(&p, &kernel_opts, None, &cancel).unwrap();
        assert_eq!(core_m.channels.len(), kernel_m.channels.len());
        assert_eq!(core_m.channels[0].stars_fitted, kernel_m.channels[0].stars_fitted);
    }

    #[test]
    fn the_adapter_reads_egain_and_cscl_into_the_gain() {
        use crate::fits_writer::{Card, CardValue};
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("g.fits");
        let (data, w, h) = crate::test_support::fixture_field(7, 1.0, 0.002);
        let cards = [
            Card::new("EGAIN", CardValue::Real(50000.0 / 65535.0)).unwrap(),
            Card::new("ATH_CSCL", CardValue::Real(65535.0)).unwrap(),
            Card::new("ATH_CFNM", CardValue::Real(3000.0)).unwrap(),
        ];
        write_fits_f32(&p, w, h, 1, &data, &cards).unwrap();
        assert!(
            (gain_from_header(&p).unwrap() - 50000.0).abs() < 1e-6,
            "EGAIN x ATH_CSCL, CFNM ignored (PA-22)"
        );
        let q = d.path().join("n.fits");
        write_fits_f32(&q, w, h, 1, &data, &[]).unwrap();
        assert_eq!(gain_from_header(&q), None);
        let r = d.path().join("only_egain.fits");
        write_fits_f32(&r, w, h, 1, &data, &cards[..1]).unwrap();
        assert_eq!(gain_from_header(&r), None, "ATH_CSCL missing: no gain");
        assert_eq!(
            gain_from_header(&d.path().join("missing.fits")),
            None,
            "an unreadable file has none (logged)"
        );
    }
}

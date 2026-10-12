//! The plane's domain (spec 2026-10-07 §2.1 `domain`): a calibrated float plane in `[0, 1]` or a raw ADU plane, the
//! module's working units, the plane's noise (MRS, with the large-scale background residual as its fallback) and the
//! stratified-sample sky statistic. `ADU_SCALE`, `NoiseSource`, `background_residual`, `noise_mrs` and their
//! constants, the stacking Measure stage's two noise blocks (→ [`noise_model`]) and its sample-statistics block (→
//! [`sky_level`], ruling F4 / PA-21) moved here verbatim in Plan A Task 2.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tracing::warn;

use super::PhaseTimings;
use crate::integration::stats::{
    self, median_in_place, LocationScale, ScaleEstimator, CLIP_HI, CLIP_LO, NOISE_CLIP_HI,
    NOISE_CLIP_LO,
};

/// Calibrated frames are float32 in `[0, 1]`; the detector and the MRS
/// estimator carry absolute floors tuned for 16-bit ADU data, so they see a
/// copy scaled by this factor. Every estimator this module reports is
/// scale-invariant (PSF Signal Weight, PSF SNR) or divided back into
/// native units.
pub const ADU_SCALE: f32 = 65535.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum NoiseSource {
    #[default]
    Mrs,
    /// MRS was unavailable; `noise` is `N*`.
    BackgroundResidual,
    /// Neither estimate measured the plane (MRS at its floor on a constant or near-constant plane, and `N*` missing
    /// or zero): `sigma` is 0, the matched filter has no threshold and seeds nothing (Review Focus 4, Plan A Task 6a).
    /// Serialized `none` in the stacking `metrics` payload (ruling F43).
    None,
}

/// What the plane handed to the module is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Domain {
    /// A calibrated float plane in [0, 1]; the module multiplies by `ADU_SCALE` exactly where the adapter did.
    Calibrated { gain_e_per_unit: Option<f64> },
    /// A raw ADU plane; `clip` = the sensor's clip level in the plane's own units (BITPIX/BZERO or DATAMAX).
    /// `full_scale` = the stored integer type's full scale, `2^bits − 1` from BITPIX/BZERO (65535 for unsigned 16-bit,
    /// 255 for 8-bit; never DATAMAX): the saturation test's `norm` (F-3, Task 8, Q2). `clip` keeps its own readers.
    RawAdu {
        clip: f64,
        full_scale: f64,
        gain_e_per_adu: Option<f64>,
    },
}

/// The plane's noise in the module's working units (ADU-scaled for a `Calibrated` plane, the plane's own ADU for
/// `RawAdu`): `sigma` is the MRS σ, or `n_star` when MRS refused the plane (`source` says which); `m_star`, `n_star`
/// are `M*`, `N*` of the large-scale background residual (math reference §1.3), zero when the background model was
/// unavailable.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoiseModel {
    pub sigma: f64,
    pub source: NoiseSource,
    pub m_star: f64,
    pub n_star: f64,
}

/// `N* = 2.48308·MAD(R)`: the MAD of a half-normal sample to its σ.
// The reference's constant; the analytic half-normal value is 2.5064 — the
// 1 % difference is deliberate parity, not an error.
pub const N_STAR_FROM_MAD: f64 = 2.48308;
/// Mesh cell of the large-scale background model (model scale ≈ 256 px).
pub const BACKGROUND_MODEL_CELL_PX: usize = 128;
pub const MRS_LAYERS: usize = 4;
/// Gaussian noise-propagation factor of the first B3-spline à-trous layer
/// (math reference §2.1): the estimator reports layer-1 coefficients, which
/// carry this fraction of the pixel noise σ.
pub const MRS_LAYER1_GAIN: f32 = 0.8907;

/// The plane's one background mesh (Plan A Task 10b, rulings A2 and Q4): the 128-px mesh model over the plane's
/// PRESENT pixels (finite and, when a mask is given, `mask[i] == true`). It is the large-scale background residual's
/// model `L` (`N*`, [`background_residual_from`]), the matched filter's background and — with `noise` — the per-cell
/// clipped σ the detection noise σ_f propagates (its noise map, [`super::seeds`]). [`super::measure_plane`] computes it
/// ONCE per plane and hands it to both; nothing builds a second one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BackgroundMesh {
    /// The per-pixel background model; `None` when the mesh model was unavailable.
    pub background: Option<Vec<f32>>,
    /// The per-pixel noise map (the bicubic interpolation of the per-cell clipped σ); `None` when it was not asked
    /// for or the model was unavailable.
    pub noise: Option<Vec<f32>>,
}

/// The mesh model (cell [`BACKGROUND_MODEL_CELL_PX`]) of `data` over its present pixels (a pixel with
/// `mask[i] == false` enters as absent, like a non-finite one), with the per-pixel noise map when `want_noise` (a
/// second bicubic interpolation pass; it changes no other field of the estimate, so the background map is the same
/// bytes either way). A geometry mismatch gives an empty mesh and a `warn!`.
pub fn background_mesh(
    data: &[f32],
    w: usize,
    h: usize,
    mask: Option<&[bool]>,
    want_noise: bool,
) -> BackgroundMesh {
    if data.len() != w * h || mask.is_some_and(|m| m.len() != w * h) {
        tracing::warn!(
            len = data.len(),
            width = w,
            height = h,
            "background mesh: plane (or mask) length does not match the geometry"
        );
        return BackgroundMesh::default();
    }
    let masked: Vec<f32>;
    let input: &[f32] = match mask {
        Some(m) => {
            masked = data
                .iter()
                .zip(m)
                .map(|(&v, &keep)| if keep { v } else { f32::NAN })
                .collect();
            &masked
        }
        None => data,
    };
    let bg = astroimage::analysis::background::estimate_background_mesh(
        input,
        w,
        h,
        BACKGROUND_MODEL_CELL_PX,
        want_noise,
    );
    BackgroundMesh {
        background: bg.background_map.filter(|m| m.len() == w * h),
        noise: bg.noise_map.filter(|m| m.len() == w * h),
    }
}

/// `M*`, `N*` of the large-scale background residual (math reference
/// §1.3): the model `L` is the mesh background (cell 128 px);
/// `R = {L − v : v ≠ 0, v < L}` over the stratified sample;
/// `M* = median(R)`, `N* = 2.48308·MAD(R)`. `None` below 100 residuals.
pub fn background_residual(data: &[f32], w: usize, h: usize) -> Option<(f64, f64)> {
    if data.len() != w * h {
        tracing::warn!(
            len = data.len(),
            width = w,
            height = h,
            "background residual: plane length does not match the geometry"
        );
        return None;
    }
    // Only the background map is read — the per-pixel noise map would cost a second bicubic interpolation pass for
    // nothing.
    let mesh = background_mesh(data, w, h, None, false);
    background_residual_from(data, w, h, mesh.background.as_deref()?)
}

/// [`background_residual`] against a model `L` already computed — the plane's one [`BackgroundMesh`]. `None` below
/// 100 residuals, or when `data` or `model` does not match the geometry.
pub fn background_residual_from(
    data: &[f32],
    w: usize,
    h: usize,
    model: &[f32],
) -> Option<(f64, f64)> {
    if data.len() != w * h || model.len() != w * h {
        tracing::warn!(
            len = data.len(),
            width = w,
            height = h,
            "background residual: plane or model length does not match the geometry"
        );
        return None;
    }
    let mut r: Vec<f32> = Vec::with_capacity(data.len() / 32);
    crate::integration::stats::for_each_stratified(w, h, |i| {
        let (v, l) = (data[i], model[i]);
        if v.is_finite() && l.is_finite() && v != 0.0 && v < l {
            r.push(l - v);
        }
    });
    if r.len() < 100 {
        return None;
    }
    let m = median_in_place(&mut r);
    let mad = crate::integration::stats::mad_about(&r, m);
    Some((m as f64, N_STAR_FROM_MAD * mad as f64))
}

/// MRS noise (`estimate_noise_mrs`, 4 layers). The estimator carries an
/// absolute floor tuned for 16-bit ADU data, so callers feed it ADU-scaled
/// values ([`ADU_SCALE`]); `None` when the result sits on that
/// floor (a constant or near-constant plane). The raw estimate is divided by
/// `MRS_LAYER1_GAIN` to recover σ from the first layer's coefficients (§2.2).
pub fn noise_mrs(data: &[f32], w: usize, h: usize) -> Option<f32> {
    let raw = astroimage::analysis::background::estimate_noise_mrs(data, w, h, MRS_LAYERS);
    if raw.is_finite() && raw > 0.002 {
        Some(raw / MRS_LAYER1_GAIN)
    } else {
        None
    }
}

/// The ADU-scaled copy of a `Calibrated` plane, or the plane itself for `RawAdu`.
pub fn to_working_units<'a>(plane: &'a [f32], domain: &Domain) -> Cow<'a, [f32]> {
    match domain {
        Domain::Calibrated { .. } => Cow::Owned(plane.iter().map(|v| v * ADU_SCALE).collect()),
        Domain::RawAdu { .. } => Cow::Borrowed(plane),
    }
}

/// `background_residual` + `noise_mrs` on the working-unit plane, in that order, with the fallbacks and `warn!`s the
/// stacking Measure stage has always applied: no background model → `M* = N* = 0`; no MRS estimate → σ = `N*`,
/// [`NoiseSource::BackgroundResidual`]. When neither measured the plane (no MRS estimate and no positive `N*`) the
/// σ is 0 and the source [`NoiseSource::None`] (Plan A Task 6a), with its own `warn!`. Both run on `pool` when given
/// one. The returned timings carry `background_ms` and `noise_ms` (`detect_ms` and `fit_ms` are 0).
pub fn noise_model(
    working: &[f32],
    w: usize,
    h: usize,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> (NoiseModel, PhaseTimings) {
    noise_model_with(working, w, h, None, pool)
}

/// [`noise_model`] with `N*` read against the plane's one [`BackgroundMesh`] when the caller already computed it
/// ([`super::measure_plane`], ruling Q4), so the mesh is not built a second time; `None` = built here, as
/// [`noise_model`] does. `background_ms` then covers the residual alone (the mesh is the caller's phase).
///
/// On a masked plane the shared mesh leaves the absent pixels out, so `N*` reads the masked mesh (ruling D-1): an
/// absent pixel is not sky. [`noise_model`] builds its own mesh over the whole plane, so the two differ there.
pub fn noise_model_with(
    working: &[f32],
    w: usize,
    h: usize,
    mesh: Option<&BackgroundMesh>,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> (NoiseModel, PhaseTimings) {
    let t = Instant::now();
    let residual = || match mesh {
        Some(m) => m
            .background
            .as_deref()
            .and_then(|model| background_residual_from(working, w, h, model)),
        None => background_residual(working, w, h),
    };
    let bg = match pool {
        Some(p) => p.install(residual),
        None => residual(),
    };
    let background_ms = t.elapsed().as_millis() as u64;
    let (m_star, n_star) = match bg {
        Some(v) => v,
        None => {
            warn!(
                width = w,
                height = h,
                "large-scale background model unavailable; M* and N* are zero"
            );
            (0.0, 0.0)
        }
    };
    let t = Instant::now();
    let mrs = match pool {
        Some(p) => p.install(|| noise_mrs(working, w, h)),
        None => noise_mrs(working, w, h),
    };
    let (sigma, source) = match mrs {
        Some(n) => (n as f64, NoiseSource::Mrs),
        None if n_star.is_finite() && n_star > 0.0 => {
            warn!(
                n_star,
                "MRS noise unavailable; using the background residual scale"
            );
            (n_star, NoiseSource::BackgroundResidual)
        }
        None => {
            warn!(
                n_star,
                width = w,
                height = h,
                "no noise estimate: MRS and the background residual both refused the plane"
            );
            (0.0, NoiseSource::None)
        }
    };
    let noise_ms = t.elapsed().as_millis() as u64;
    (
        NoiseModel {
            sigma,
            source,
            m_star,
            n_star,
        },
        PhaseTimings {
            background_ms,
            noise_ms,
            detect_ms: 0,
            fit_ms: 0,
        },
    )
}

/// The stratified-sample sky statistic (I-8, PA-21) in the plane's NATIVE units: the median, MAD and mean absolute
/// deviation of the clipped sample, the normalization location and two-sided scale, and the noise-scale factors.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkyLevel {
    pub median: f64,
    pub mad: f64,
    pub median_mean_dev: f64,
    pub location: f64,
    pub scale: f64,
    pub noise_scale_low: f64,
    pub noise_scale_high: f64,
}

/// The stratified-sample sky statistic (`stats::stratified_sample` → `clip_sample` → median / MAD / avg-dev /
/// `location_scale` / `noise_scale_factors`) on the NATIVE plane, moved from the stacking Measure stage so every
/// consumer reads one value (I-8, PA-21). The clip range is the domain's: `Calibrated` → `[CLIP_LO, CLIP_HI]` with
/// the noise-scale factors on `[NOISE_CLIP_LO, NOISE_CLIP_HI]` (today's bytes, today's numbers); `RawAdu { clip }` →
/// `[1, clip − 1]` with the noise-scale factors on `[2, clip − 2]`, the same one- and two-count margins in the
/// plane's own ADU.
pub fn sky_level(
    native: &[f32],
    w: usize,
    h: usize,
    domain: &Domain,
    est: ScaleEstimator,
) -> SkyLevel {
    let sample = stats::stratified_sample(native, w, h);
    let (lo, hi, noise_lo, noise_hi) = match domain {
        Domain::Calibrated { .. } => (CLIP_LO, CLIP_HI, NOISE_CLIP_LO, NOISE_CLIP_HI),
        Domain::RawAdu { clip, .. } => (1.0, (clip - 1.0) as f32, 2.0, (clip - 2.0) as f32),
    };
    let clipped = stats::clip_sample(&sample, lo, hi);
    let (median, mad, median_mean_dev) = if clipped.is_empty() {
        match domain {
            Domain::Calibrated { .. } => warn!(
                samples = sample.len(),
                "no in-range samples after clipping; the plane may not be in [0, 1]"
            ),
            Domain::RawAdu { clip, .. } => warn!(
                samples = sample.len(),
                clip = *clip,
                "no in-range samples after clipping; the plane may not be in [1, clip - 1] ADU"
            ),
        }
        (0.0, 0.0, 0.0)
    } else {
        let m = stats::median_of(&clipped);
        (
            m as f64,
            stats::mad_about(&clipped, m) as f64,
            stats::avg_dev_about(&clipped, m) as f64,
        )
    };
    let ls = stats::location_scale(&clipped, est).unwrap_or(LocationScale {
        location: median as f32,
        scale: 0.0,
    });
    let (noise_scale_low, noise_scale_high) =
        stats::noise_scale_factors_within(&sample, noise_lo, noise_hi)
            .map(|(a, b)| (a as f64, b as f64))
            .unwrap_or((0.0, 0.0));
    SkyLevel {
        median,
        mad,
        median_mean_dev,
        location: ls.location as f64,
        scale: ls.scale as f64,
        noise_scale_low,
        noise_scale_high,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{add_noise, gaussian_field};

    #[test]
    fn background_residual_of_pure_noise_is_half_normal() {
        let (w, h) = (512, 512);
        let mut data = vec![0.1f32; w * h];
        for y in 0..h {
            for x in 0..w {
                data[y * w + x] += 0.005 * x as f32 / w as f32; // a 5 % gradient the model must follow
            }
        }
        add_noise(&mut data, 0.01, 21);
        let (m, n) = background_residual(&data, w, h).unwrap();
        assert!((m - 0.006745).abs() < 0.15 * 0.006745, "M* {m}");
        assert!((n - 0.01).abs() < 0.10 * 0.01, "N* {n}");
        assert!(background_residual(&[0.5; 64], 8, 8).is_none());
    }

    #[test]
    fn background_residual_refuses_a_length_mismatch() {
        // A length that would read out of bounds without the guard — the
        // stratified sample over 512x512 touches indices near `w * h - 1`,
        // so a caller-supplied buffer one short of `w * h` used to panic.
        assert!(background_residual(&vec![0.0f32; 512 * 512 - 1], 512, 512).is_none());
    }

    #[test]
    fn mrs_noise_tracks_the_true_sigma_and_survives_stars() {
        let (w, h) = (512, 512);
        let mut plain = vec![1000.0f32; w * h]; // ADU-scaled units, 20 ADU of noise
        add_noise(&mut plain, 20.0, 31);
        let n = noise_mrs(&plain, w, h).unwrap();
        assert!((n - 20.0).abs() < 0.05 * 20.0, "pure noise: {n}");
        let stars: Vec<(f64, f64, f64)> = (0..200)
            .map(|i| {
                (
                    16.0 + (i % 20) as f64 * 24.0,
                    16.0 + (i / 20) as f64 * 48.0,
                    500.0 + 100.0 * (i % 7) as f64,
                )
            })
            .collect();
        let mut field = gaussian_field(w, h, &stars, 2.0, 1000.0);
        add_noise(&mut field, 20.0, 32);
        let n = noise_mrs(&field, w, h).unwrap();
        assert!((n - 20.0).abs() < 0.08 * 20.0, "star field: {n}");
        assert!(noise_mrs(&[5.0; 256], 16, 16).is_none());
    }
}

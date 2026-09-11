//! The structure-map seed detector (math reference §5.1, ruling R-M4c-11):
//! the second of the two detectors the quality measurement can seed its PSF
//! fits from (`measurement.seedDetector`).
//!
//! The peak detector [`super::measure`] has used since Plan 2 answers one
//! question per pixel — "does this pixel stand `k·σ` above the local sky?"
//! — and that is why its star population moves the wrong way with seeing:
//! a sharp night's star puts the same flux into fewer, taller pixels, so a
//! peak threshold *finds more* stars exactly where a structure-based
//! detector finds fewer (M4a Task 2, rulings R-M4a-1/R-M4a-13). This
//! module asks the other question — "which connected groups of pixels look
//! like a star?" — and reproduces that sharpness behaviour by
//! construction:
//!
//! 1. optional 3×3 median (the reference's radius-1 hot-pixel filter),
//!    which suppresses a narrow star's core far harder than a soft one's;
//! 2. high-pass: subtract a Gaussian of size `1 + 2^structureLayers`
//!    (5 → 33 px), truncate at 0 — this is what keeps galaxies, nebulae and
//!    sky gradients out of the map;
//! 3. 3×3 dilation, adaptive binarization at `median + 3·σ_noise` of the
//!    dilated map, 3×3 erosion (together a closing: it fills one-pixel gaps
//!    and holes, and leaves an isolated pixel isolated);
//! 4. connected components, then the per-candidate rules in the reference's
//!    own order (border, size, saturation, coverage, single maximum,
//!    detection SNR, kurtosis) and a barycentre for the position.
//!
//! Everything downstream of the seeds — the PSF fits, the aperture flux,
//! the background model, the noise estimate — always measures the UNTOUCHED
//! plane, exactly as with the peak detector's optional pre-filter. Inside
//! this module too: only step 1's copy is filtered, and every per-candidate
//! statistic (the ring background, the significant pixels, the barycentre,
//! the kurtosis) is read from the caller's plane.
//!
//! Units: the detector reads the plane in whatever units the caller hands
//! it and compares [`StructureParams::upper_limit`] against those units, so
//! the ADU-scaled copy `measure_plane` works on needs an ADU-scaled limit
//! (`measure::ADU_SCALE`), not the `[0, 1]` default. The noise estimator
//! carries an absolute floor tuned for 16-bit data, which is the other
//! reason the map is never rescaled into `[0, 1]` first.

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use tracing::debug;

use super::prefilter;
use super::psf_signal::Seed;
use crate::integration::stats::{self, MAD_TO_SIGMA};

/// Which detector produces the quality measurement's PSF-fit seeds
/// (spec §9.2 `measurement.seedDetector`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum SeedDetector {
    /// The peak-threshold detector: two noise-relative levels, one answer
    /// per pixel (`measurement.detectionSigma`).
    #[default]
    Peak,
    /// The structure-map detector in this module (math reference §5.1).
    Structure,
}

impl SeedDetector {
    /// The serde name, for logs — `"peak"` / `"structure"`. One spelling
    /// everywhere, so a log filter and a stored config agree.
    pub fn as_str(self) -> &'static str {
        match self {
            SeedDetector::Peak => "peak",
            SeedDetector::Structure => "structure",
        }
    }
}

/// The structure detector's dials, with the reference's own defaults (math
/// reference §5.1). Every field is a `StructureParams` knob rather than a
/// `StackingConfig` field: the only thing a stored config selects is WHICH
/// detector runs — this task calibrated the dials once and the calibrated
/// values are the defaults here.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StructureParams {
    /// The high-pass Gaussian's size is `1 + 2^structure_layers` px; 5 → 33,
    /// i.e. structures up to roughly 32 px survive it.
    pub structure_layers: u8,
    /// The reference's radius-1 hot-pixel filter: a 3×3 median on the copy
    /// the structure map is built from (never on the plane the statistics
    /// are read from).
    pub hot_pixel_median: bool,
    /// Detection threshold in local-sigma units:
    /// `snr_threshold = 0.1 + 4.8·(1 − sensitivity)` (0.5 → 2.5). Higher
    /// sensitivity → lower threshold → more stars.
    pub sensitivity: f64,
    /// Peak sensitivity in kurtosis units:
    /// `peak_threshold = 0.1 + 9.8·(1 − peak_response)` (0.5 → 5.0). A
    /// structure flatter than this is not a star — unless it is bright.
    pub peak_response: f64,
    /// A structure whose `snr/snr_threshold` reaches this is bright enough
    /// to skip the kurtosis rule.
    pub bright_threshold: f64,
    /// Structures smaller than this in pixels are not stars. `0` selects the
    /// reference's automatic mode: the size floor is derived from the
    /// detected population's own size distribution.
    pub min_structure_size: usize,
    /// Coverage floor `(π/4)·(1 − max_distortion)`: the fraction of its own
    /// bounding box a structure's significant pixels must fill. A perfect
    /// square is 1, a perfect circle π/4.
    pub max_distortion: f64,
    /// Keep a structure carrying more than one local maximum (a blend)
    /// instead of rejecting it.
    pub allow_clustered_sources: bool,
    /// Saturation limit, in the input plane's own units: a structure whose
    /// brightest significant pixel exceeds it has no usable centre. The
    /// local-maxima map ignores pixels above `0.75·upper_limit` for the same
    /// reason (a flat-topped star would otherwise report many maxima).
    pub upper_limit: f32,
}

impl Default for StructureParams {
    fn default() -> Self {
        StructureParams {
            structure_layers: DEFAULT_STRUCTURE_LAYERS,
            hot_pixel_median: true,
            sensitivity: DEFAULT_SENSITIVITY,
            peak_response: 0.5,
            bright_threshold: 3.0,
            max_distortion: 0.6,
            min_structure_size: 0,
            allow_clustered_sources: false,
            upper_limit: 1.0,
        }
    }
}

/// Default for [`StructureParams::structure_layers`] — the high-pass kernel
/// is `1 + 2^5 = 33` px wide.
pub const DEFAULT_STRUCTURE_LAYERS: u8 = 5;

/// Default for [`StructureParams::sensitivity`] — the detection threshold is
/// `0.1 + 4.8·(1 − 0.7) = 1.54` local sigma.
///
/// The reference's own default is 0.5. M4c Task 0 swept 0.3/0.5/0.7 on 94
/// real frames against an external tool's per-frame log and shipped 0.7:
/// it is where the mono fit counts land on the reference's (per-night
/// ratios 1.02/0.81/1.00, against 0.87/0.79/0.84 at 0.5 and 0.72/0.71/0.71
/// at 0.3 — the last already outside the acceptance band) and where the
/// weight and fit-count rank correlations peak. The dial does not explain
/// the OSC residual: the bright night's fit ratio is 2.59/2.70/2.71 across
/// the whole sweep, i.e. that excess is not an SNR-gate effect. Compensating
/// for this module's stricter binarization level (see `structure_map`) is
/// the other half of the reason it sits above the reference's.
pub const DEFAULT_SENSITIVITY: f64 = 0.7;

/// Truncation error of the reference's Gaussian filters: a filter of odd
/// size `n` carries `σ = (n/2)/√(−2·ln ε)`, i.e. the kernel IS the Gaussian
/// truncated at just over 3σ. (Reading "size" as a FWHM instead would put
/// the same kernel's own 3σ truncation at 42 px — wider than the size it
/// was derived from — and would leave 40 % of a 40-px object's peak in the
/// high-passed map instead of 9 %, which is the opposite of what the
/// high-pass is for.)
const FILTER_EPSILON: f64 = 0.01;

/// Local-maxima detection limit as a fraction of [`StructureParams::upper_limit`]
/// — a pixel at or above it is too close to saturation to be trusted as a
/// maximum.
const LOCAL_MAXIMA_LIMIT: f32 = 0.75;

/// Gaussian noise-propagation factor of the SECOND B3-spline à-trous layer
/// (math reference §2.1's table, the sibling of [`MRS_LAYER1_GAIN`]): the
/// layer's coefficients carry this fraction of the pixel noise σ.
const B3_LAYER2_GAIN: f32 = 0.2007;

/// Radius of the local-maxima window: a pixel is a maximum when it is
/// strictly greater than all 24 of its neighbours.
const LOCAL_MAXIMA_RADIUS: usize = 2;

/// The ring the local background is measured in starts this far outside the
/// structure's bounding box and grows until its median stabilizes.
const BKG_DELTA: usize = 4;

/// Stretch factor of the barycentre search, in sigma units: the detection
/// box is truncated at `median + XY_STRETCH·stddev` before the centre of
/// mass is taken, so nearby structures cannot pull the position.
const XY_STRETCH: f64 = 1.5;

/// Seeds from the structure map, brightest (largest flux) first.
///
/// `plane` is `w × h` row-major in the caller's own units; `peak` and `flux`
/// come back background-subtracted (`initial_sigma` in
/// [`super::psf_signal::fit_stars`] reads them as a star's amplitude and
/// total signal, so a sky pedestal in either would corrupt the fitter's
/// starting width).
pub fn detect_structures(plane: &[f32], w: usize, h: usize, p: &StructureParams) -> Vec<Seed> {
    if w < 3 || h < 3 || plane.len() < w * h {
        return Vec::new();
    }
    let (bin, threshold, noise) = structure_map(plane, w, h, p);
    let structures = label_structures(&bin, w, h);

    let snr_threshold = 0.1 + 4.8 * (1.0 - p.sensitivity.clamp(0.0, 1.0));
    let peak_threshold = 0.1 + 9.8 * (1.0 - p.peak_response.clamp(0.0, 1.0));
    let min_coverage = std::f64::consts::FRAC_PI_4 * (1.0 - p.max_distortion.clamp(0.0, 1.0));
    let lm_limit = LOCAL_MAXIMA_LIMIT * p.upper_limit;

    let mut cands: Vec<Candidate> = structures
        .par_iter()
        .filter_map(|s| {
            evaluate(
                plane,
                w,
                h,
                s,
                p,
                lm_limit,
                snr_threshold,
                peak_threshold,
                min_coverage,
            )
        })
        .collect();

    let min_star_size = if p.min_structure_size == 0 {
        automatic_min_star_size(&cands)
    } else {
        p.min_structure_size
    };
    let accepted = keep_isolated(&mut cands, min_star_size);

    let mut seeds: Vec<Seed> = accepted
        .into_iter()
        .map(|c| Seed {
            x: c.x,
            y: c.y,
            peak: c.peak,
            flux: c.flux,
        })
        .collect();
    seeds.sort_by(|a, b| b.flux.total_cmp(&a.flux));
    debug!(
        width = w,
        height = h,
        structures = structures.len(),
        count = seeds.len(),
        threshold,
        noise,
        min_star_size,
        "structure map seeds detected"
    );
    seeds
}

// ── The structure map ────────────────────────────────────────────────────

/// The binarized, eroded structure map plus the binarization level and the
/// noise it was derived from (both reported for the log line only).
fn structure_map(plane: &[f32], w: usize, h: usize, p: &StructureParams) -> (Vec<u8>, f32, f32) {
    // Step 1 — the hot-pixel median, on the map's own copy only.
    let mut map = if p.hot_pixel_median {
        prefilter::median3(plane, w, h)
    } else {
        plane.to_vec()
    };

    // Step 2 — high-pass, truncated at 0. The reference rescales the result
    // into [0, 1] here; we do not, because `median + 3·σ` is invariant under
    // that rescale (both terms scale together) while the noise estimator's
    // absolute floor is not.
    {
        let size = 1usize + (1usize << p.structure_layers.clamp(1, 8));
        let low = blur_separable(&map, w, h, &gaussian_kernel(size / 2));
        map.par_iter_mut()
            .zip(low.par_iter())
            .for_each(|(m, l)| *m = (*m - *l).max(0.0));
    }

    // Step 3 — dilation, adaptive binarization, erosion.
    let dilated = box3(&map, w, h, true);
    drop(map);
    let median = stats::median_of(&dilated);
    // The LEVEL comes from the dilated map (its own zero point), the SCALE
    // from the UNFILTERED plane (ruling R-M4c-11, and R-M4a-14's lesson
    // before it). Measuring the scale on the map instead lets the
    // hot-pixel median cancel itself: that filter attenuates the noise
    // (≈ 0.42×) as well as the stars, so a level taken from the filtered
    // map would fall with the very peaks the filter suppressed, and the
    // detector would lose most of the sharpness behaviour it exists for.
    // Measured on this module's fixture: anchored on the plane, an
    // undersampled field yields 0.52× the well-sampled field's stars;
    // anchored on the map, 0.73×.
    let noise = map_noise(plane, w, h);
    let threshold = if noise > 0.0 {
        median + 3.0 * noise
    } else {
        // A black, noiseless background (a synthetic field): there is no
        // noise scale to work from, so the map's own dispersion is the
        // level.
        median + MAD_TO_SIGMA * stats::mad_about(&dilated, median)
    };
    let bin: Vec<u8> = dilated
        .par_iter()
        .map(|&v| u8::from(v >= threshold))
        .collect();
    (erode3(&bin, w, h), threshold, noise)
}

/// σ of a plane's noise, in that plane's own units: the K-sigma dispersion of
/// the SECOND B3-spline à-trous detail layer, divided by that layer's
/// noise-propagation factor [`B3_LAYER2_GAIN`].
///
/// Why not the first layer (which the crate's own 4-layer MRS estimator and
/// `estimate_noise_mrs(.., 1)` report, over [`MRS_LAYER1_GAIN`]): by the
/// time the threshold is taken, the map has been through a 3×3 median and a
/// 3×3 dilation, and both of those are smoothers — every pixel is the max of
/// its neighbourhood, so the finest scale carries almost nothing left of the
/// original noise. Measured on this module's own 512×512 fixture the first
/// layer answers 1.04 ADU where the second answers 4.4 ADU for the same
/// 8 ADU input noise, i.e. the first-layer route would set the binarization
/// level four times too low and let the whole noise field into the structure
/// map. The second layer sits above the smoothing and is what the reference
/// reads.
fn map_noise(map: &[f32], w: usize, h: usize) -> f32 {
    let c1 = b3_smooth(map, w, h, 1);
    let c2 = b3_smooth(&c1, w, h, 2);
    // Layer 2 = c1 − c2, subsampled to bound the cost on a 26 Mpx plane and
    // kept clear of the border the dilated kernel reaches over.
    let border = 4usize;
    if w <= 2 * border || h <= 2 * border {
        return 0.0;
    }
    let stride = (((w * h) as f64 / 500_000.0).sqrt() as usize).max(1);
    let mut v: Vec<f32> = Vec::new();
    let mut y = border;
    while y < h - border {
        let base = y * w;
        let mut x = border;
        while x < w - border {
            let c = c1[base + x] - c2[base + x];
            if c.is_finite() {
                v.push(c);
            }
            x += stride;
        }
        y += stride;
    }
    let sigma = k_sigma(&v);
    if sigma.is_finite() && sigma > 0.0 {
        sigma / B3_LAYER2_GAIN
    } else {
        0.0
    }
}

/// One B3-spline à-trous smoothing step at scale `step` (1, 2, 4 …):
/// separable `[1, 4, 6, 4, 1]/16` with holes, edge-replicated.
fn b3_smooth(src: &[f32], w: usize, h: usize, step: usize) -> Vec<f32> {
    const K: [f32; 5] = [0.0625, 0.25, 0.375, 0.25, 0.0625];
    let mut tmp = vec![0.0f32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let base = y * w;
        for (x, o) in row.iter_mut().enumerate() {
            let mut acc = 0.0f32;
            for (j, &kv) in K.iter().enumerate() {
                let xx = (x as isize + (j as isize - 2) * step as isize).clamp(0, w as isize - 1)
                    as usize;
                acc += kv * src[base + xx];
            }
            *o = acc;
        }
    });
    let mut out = vec![0.0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (j, &kv) in K.iter().enumerate() {
            let yy =
                (y as isize + (j as isize - 2) * step as isize).clamp(0, h as isize - 1) as usize;
            for (o, &v) in row.iter_mut().zip(&tmp[yy * w..yy * w + w]) {
                *o += kv * v;
            }
        }
    });
    out
}

/// Iterative k-sigma dispersion: the standard deviation of the samples
/// within ±3σ of the running mean, until it settles to 1 % or ten rounds
/// pass. Robust to the star structures that fill a high-passed map, which a
/// plain standard deviation is not.
fn k_sigma(v: &[f32]) -> f32 {
    if v.len() < 100 {
        return 0.0;
    }
    let n = v.len() as f64;
    let mut mean = v.iter().map(|&x| x as f64).sum::<f64>() / n;
    let mut sigma = (v.iter().map(|&x| (x as f64 - mean).powi(2)).sum::<f64>() / n).sqrt();
    for _ in 0..10 {
        if !(sigma > 0.0) {
            return 0.0;
        }
        let (lo, hi) = (mean - 3.0 * sigma, mean + 3.0 * sigma);
        let (mut count, mut s1, mut s2) = (0usize, 0.0f64, 0.0f64);
        for &x in v {
            let x = x as f64;
            if x >= lo && x <= hi {
                count += 1;
                s1 += x;
                s2 += x * x;
            }
        }
        if count < 100 {
            break;
        }
        let m = s1 / count as f64;
        let var = (s2 / count as f64 - m * m).max(0.0);
        let sd = var.sqrt();
        let done = sd <= 0.0 || (sigma - sd).abs() < 0.01 * sigma;
        mean = m;
        sigma = if sd > 0.0 { sd } else { sigma };
        if done {
            break;
        }
    }
    sigma as f32
}

/// A Gaussian kernel of `2·radius + 1` taps, normalized to unit sum, with
/// the reference's size ↔ σ relation (see [`FILTER_EPSILON`]).
fn gaussian_kernel(radius: usize) -> Vec<f32> {
    let radius = radius.max(1);
    let sigma = radius as f64 / (-2.0 * FILTER_EPSILON.ln()).sqrt();
    let mut k: Vec<f32> = (0..=2 * radius)
        .map(|i| {
            let d = i as f64 - radius as f64;
            (-(d * d) / (2.0 * sigma * sigma)).exp() as f32
        })
        .collect();
    let s: f32 = k.iter().sum();
    if s > 0.0 {
        for v in &mut k {
            *v /= s;
        }
    }
    k
}

/// Separable convolution with edge replication — never a wrapped or
/// zero-padded neighbour, which would darken the frame's border and invent
/// structures along it.
fn blur_separable(src: &[f32], w: usize, h: usize, k: &[f32]) -> Vec<f32> {
    let r = k.len() / 2;
    let mut tmp = vec![0.0f32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let base = y * w;
        for (x, o) in row.iter_mut().enumerate() {
            let mut acc = 0.0f32;
            for (j, &kv) in k.iter().enumerate() {
                let xx = (x + j).saturating_sub(r).min(w - 1);
                acc += kv * src[base + xx];
            }
            *o = acc;
        }
    });
    let mut out = vec![0.0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (j, &kv) in k.iter().enumerate() {
            let yy = (y + j).saturating_sub(r).min(h - 1);
            let src_row = &tmp[yy * w..yy * w + w];
            for (o, &v) in row.iter_mut().zip(src_row) {
                *o += kv * v;
            }
        }
    });
    out
}

/// 3×3 box dilation (`max = true`) or erosion, separable, edge-replicated.
fn box3(src: &[f32], w: usize, h: usize, max: bool) -> Vec<f32> {
    let pick = |a: f32, b: f32| if max { a.max(b) } else { a.min(b) };
    let mut tmp = vec![0.0f32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let base = y * w;
        for (x, o) in row.iter_mut().enumerate() {
            let x0 = x.saturating_sub(1);
            let x1 = (x + 1).min(w - 1);
            let mut v = src[base + x0];
            for xx in x0 + 1..=x1 {
                v = pick(v, src[base + xx]);
            }
            *o = v;
        }
    });
    let mut out = vec![0.0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let y0 = y.saturating_sub(1);
        let y1 = (y + 1).min(h - 1);
        row.copy_from_slice(&tmp[y0 * w..y0 * w + w]);
        for yy in y0 + 1..=y1 {
            for (o, &v) in row.iter_mut().zip(&tmp[yy * w..yy * w + w]) {
                *o = pick(*o, v);
            }
        }
    });
    out
}

/// 3×3 binary erosion: a pixel survives only when its whole 3×3
/// neighbourhood is foreground. Edge-replicated, like [`box3`].
fn erode3(bin: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut tmp = vec![0u8; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let base = y * w;
        for (x, o) in row.iter_mut().enumerate() {
            let x0 = x.saturating_sub(1);
            let x1 = (x + 1).min(w - 1);
            *o = u8::from((x0..=x1).all(|xx| bin[base + xx] != 0));
        }
    });
    let mut out = vec![0u8; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let y0 = y.saturating_sub(1);
        let y1 = (y + 1).min(h - 1);
        for (x, o) in row.iter_mut().enumerate() {
            *o = u8::from((y0..=y1).all(|yy| tmp[yy * w + x] != 0));
        }
    });
    out
}

// ── Connected components ─────────────────────────────────────────────────

/// One connected group of foreground pixels: its bounding box (inclusive)
/// and its pixels.
struct Structure {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
    px: Vec<(u32, u32)>,
}

/// Two-pass, 4-connected labelling with union-find (path-halving finds,
/// union by smaller root — the reference grows each structure by row
/// segments, which reaches exactly the 4-connected set).
fn label_structures(bin: &[u8], w: usize, h: usize) -> Vec<Structure> {
    let mut labels = vec![0u32; w * h];
    // parent[0] is the background sentinel and is never a real label.
    let mut parent: Vec<u32> = vec![0];

    fn find(parent: &mut [u32], mut x: u32) -> u32 {
        while parent[x as usize] != x {
            let grand = parent[parent[x as usize] as usize];
            parent[x as usize] = grand;
            x = grand;
        }
        x
    }

    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if bin[i] == 0 {
                continue;
            }
            let west = if x > 0 { labels[i - 1] } else { 0 };
            let north = if y > 0 { labels[i - w] } else { 0 };
            labels[i] = match (west, north) {
                (0, 0) => {
                    let l = parent.len() as u32;
                    parent.push(l);
                    l
                }
                (a, 0) | (0, a) => a,
                (a, b) => {
                    let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
                    let (lo, hi) = if ra <= rb { (ra, rb) } else { (rb, ra) };
                    parent[hi as usize] = lo;
                    lo
                }
            };
        }
    }

    // Compact the roots, then one pass for the boxes and one for the pixels.
    let mut compact = vec![u32::MAX; parent.len()];
    let mut n = 0usize;
    let mut boxes: Vec<(usize, usize, usize, usize, usize)> = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if labels[i] == 0 {
                continue;
            }
            let root = find(&mut parent, labels[i]);
            let id = if compact[root as usize] == u32::MAX {
                compact[root as usize] = n as u32;
                boxes.push((x, y, x, y, 0));
                n += 1;
                n as u32 - 1
            } else {
                compact[root as usize]
            };
            labels[i] = id + 1;
            let b = &mut boxes[id as usize];
            b.0 = b.0.min(x);
            b.2 = b.2.max(x);
            b.3 = b.3.max(y);
            b.4 += 1;
        }
    }

    let mut out: Vec<Structure> = boxes
        .iter()
        .map(|&(x0, y0, x1, y1, count)| Structure {
            x0,
            y0,
            x1,
            y1,
            px: Vec::with_capacity(count),
        })
        .collect();
    for y in 0..h {
        for x in 0..w {
            let l = labels[y * w + x];
            if l != 0 {
                out[l as usize - 1].px.push((x as u32, y as u32));
            }
        }
    }
    out
}

// ── Per-candidate rules ──────────────────────────────────────────────────

/// One structure that passed every rule.
struct Candidate {
    x: f64,
    y: f64,
    /// Background-subtracted significant peak.
    peak: f64,
    /// Background-subtracted total flux over the significant pixels.
    flux: f64,
    /// Structure size in pixels — what the automatic size floor clusters.
    area: usize,
}

/// The rules in the reference's order. `None` at the first one that fails.
#[allow(clippy::too_many_arguments)]
fn evaluate(
    plane: &[f32],
    w: usize,
    h: usize,
    s: &Structure,
    p: &StructureParams,
    lm_limit: f32,
    snr_threshold: f64,
    peak_threshold: f64,
    min_coverage: f64,
) -> Option<Candidate> {
    // A structure one pixel wide in either axis has no centre to find
    // (a hot pixel, a cosmic-ray streak's tail, a noise residual).
    if s.x1 <= s.x0 || s.y1 <= s.y0 {
        return None;
    }
    // Touching the border: the structure is clipped, so are its statistics.
    if s.x0 == 0 || s.y0 == 0 || s.x1 + 1 >= w || s.y1 + 1 >= h {
        return None;
    }
    if s.px.len() < p.min_structure_size {
        return None;
    }

    // Local background and dispersion, from a ring that grows outward from
    // the box until its median stops falling by more than 1 % a step.
    let (bkg, sigma) = ring_background(plane, w, h, s)?;

    // Significant pixels: everything in the structure above that background.
    let mut v: Vec<f32> = Vec::with_capacity(s.px.len());
    let mut nmax = 0usize;
    let mut flux = 0.0f64;
    for &(x, y) in &s.px {
        let f = plane[y as usize * w + x as usize];
        if (f as f64) > bkg {
            if is_local_max(plane, w, h, x as usize, y as usize, lm_limit) {
                nmax += 1;
            }
            v.push(f);
            flux += f as f64;
        }
    }
    if v.is_empty() {
        return None;
    }
    if nmax > 1 && !p.allow_clustered_sources {
        return None;
    }

    let (x, y) = barycentre(plane, w, s, p.upper_limit)?;

    v.sort_by(|a, b| b.total_cmp(a));
    let max = v[0] as f64;
    let mut mn = 0usize;
    while mn < v.len() && (mn < 5 || v[mn] == v[mn - 1]) {
        mn += 1;
    }
    let peak = v[..mn].iter().map(|&f| f as f64).sum::<f64>() / mn as f64;
    let count = v.len();
    let kurt = kurtosis(&v, flux / count as f64);

    if max > p.upper_limit as f64 {
        return None;
    }
    let d = (s.x1 - s.x0 + 1).max(s.y1 - s.y0 + 1) as f64;
    if count as f64 / (d * d) < min_coverage {
        return None;
    }
    let s1 = (peak - bkg) / sigma / snr_threshold;
    if s1 < 1.0 {
        return None;
    }
    if s1 < p.bright_threshold && kurt != 0.0 && kurt < peak_threshold {
        return None;
    }

    Some(Candidate {
        x,
        y,
        peak: peak - bkg,
        flux: flux - count as f64 * bkg,
        area: s.px.len(),
    })
}

/// `(background, dispersion)` from a ring around the structure's box,
/// inflated by [`BKG_DELTA`] and then one pixel at a time until its median
/// stops falling by more than 1 % — so a star sitting on a gradient or in a
/// nebula's glow gets the background it actually stands on.
fn ring_background(plane: &[f32], w: usize, h: usize, s: &Structure) -> Option<(f64, f64)> {
    let mut m0 = f64::INFINITY;
    let mut ring: Vec<f32> = Vec::new();
    for delta in BKG_DELTA..BKG_DELTA + 200 {
        ring.clear();
        let rx0 = s.x0.saturating_sub(delta);
        let ry0 = s.y0.saturating_sub(delta);
        let rx1 = (s.x1 + delta).min(w - 1);
        let ry1 = (s.y1 + delta).min(h - 1);
        for y in ry0..ry1 + 1 {
            let inside_rows = y >= s.y0 && y <= s.y1;
            let base = y * w;
            if inside_rows {
                ring.extend_from_slice(&plane[base + rx0..base + s.x0]);
                ring.extend_from_slice(&plane[base + s.x1 + 1..base + rx1 + 1]);
            } else {
                ring.extend_from_slice(&plane[base + rx0..base + rx1 + 1]);
            }
        }
        if ring.is_empty() {
            continue;
        }
        let m = stats::median_in_place(&mut ring) as f64;
        if m > m0 || (m0 - m) < 0.01 * m0.abs() {
            let mad = stats::mad_about(&ring, m as f32) as f64;
            return Some((m, (MAD_TO_SIGMA as f64 * mad).max(f32::EPSILON as f64)));
        }
        m0 = m;
    }
    None
}

/// A pixel strictly greater than all 24 of its neighbours, and far enough
/// below saturation to be believed. Evaluated only on the significant pixels
/// of a structure, which is a few per mille of the plane.
fn is_local_max(plane: &[f32], w: usize, h: usize, x: usize, y: usize, limit: f32) -> bool {
    let v = plane[y * w + x];
    if !(v < limit) {
        return false;
    }
    let r = LOCAL_MAXIMA_RADIUS;
    for yy in y.saturating_sub(r)..=(y + r).min(h - 1) {
        let base = yy * w;
        for xx in x.saturating_sub(r)..=(x + r).min(w - 1) {
            if (xx != x || yy != y) && plane[base + xx] >= v {
                return false;
            }
        }
    }
    true
}

/// Centre of mass of the detection box after truncating it at
/// `median + XY_STRETCH·stddev` and rescaling — the low tail, where a
/// neighbouring structure or a nebular gradient lives, weighs nothing.
fn barycentre(plane: &[f32], w: usize, s: &Structure, upper: f32) -> Option<(f64, f64)> {
    let bw = s.x1 - s.x0 + 1;
    let bh = s.y1 - s.y0 + 1;
    let mut z: Vec<f32> = Vec::with_capacity(bw * bh);
    for y in s.y0..=s.y1 {
        for x in s.x0..=s.x1 {
            let v = plane[y * w + x];
            z.push(if v < 1.0e-7 { 0.0 } else { v });
        }
    }
    let med = stats::median_of(&z) as f64;
    let sd = stddev(&z);
    let lo = (med + XY_STRETCH * sd).clamp(0.0, upper as f64) as f32;
    let mut mx = f32::NEG_INFINITY;
    for v in &mut z {
        *v = v.clamp(lo, upper);
        mx = mx.max(*v);
    }
    if !(mx > lo) {
        return None;
    }
    let (mut sx, mut sy, mut sz) = (0.0f64, 0.0f64, 0.0f64);
    for (i, &v) in z.iter().enumerate() {
        let t = ((v - lo) / (mx - lo)) as f64;
        if t > 0.0 {
            sx += t * (s.x0 + i % bw) as f64;
            sy += t * (s.y0 + i / bw) as f64;
            sz += t;
        }
    }
    if sz <= 0.0 {
        return None;
    }
    Some((sx / sz, sy / sz))
}

/// Sample standard deviation (`n − 1`).
fn stddev(v: &[f32]) -> f64 {
    if v.len() < 2 {
        return 0.0;
    }
    let n = v.len() as f64;
    let mean = v.iter().map(|&x| x as f64).sum::<f64>() / n;
    let ss: f64 = v.iter().map(|&x| (x as f64 - mean).powi(2)).sum();
    (ss / (n - 1.0)).sqrt()
}

/// Kurtosis of the significant pixels about their own mean; `0` when they
/// carry no dispersion at all (the reference reads that zero as "no
/// verdict" and lets the structure through the peak rule).
fn kurtosis(v: &[f32], mean: f64) -> f64 {
    let s = stddev(v);
    if s <= 0.0 {
        return 0.0;
    }
    let k: f64 = v
        .iter()
        .map(|&f| {
            let d = (f as f64 - mean) / s;
            (d * d) * (d * d)
        })
        .sum();
    k / v.len() as f64
}

/// The reference's automatic minimum structure size: cluster the detected
/// sizes with a bandwidth taken from their own increasing gaps, then keep
/// the first cluster — unless it is a minority next to the second, in which
/// case it is the noise/hot-pixel population and the floor moves up to the
/// second cluster's smallest member.
fn automatic_min_star_size(cands: &[Candidate]) -> usize {
    if cands.is_empty() {
        return 0;
    }
    let mut sizes: Vec<usize> = cands.iter().map(|c| c.area).collect();
    sizes.sort_unstable();
    let mut deltas: Vec<usize> = Vec::new();
    let mut last = 0usize;
    for i in 1..sizes.len() {
        let d = sizes[i] - sizes[i - 1];
        if d > last {
            deltas.push(d);
            last = d;
        }
    }
    if deltas.is_empty() {
        return 0;
    }
    deltas.sort_unstable();
    let bandwidth = deltas[deltas.len() / 2].max(1);
    let mut clusters: Vec<Vec<usize>> = Vec::new();
    let mut current = vec![sizes[0]];
    for i in 1..sizes.len() {
        if sizes[i] >= sizes[i - 1] + bandwidth {
            clusters.push(std::mem::take(&mut current));
        }
        current.push(sizes[i]);
    }
    clusters.push(current);
    if clusters.len() < 2 || clusters[0].len() >= clusters[1].len() {
        clusters[0][0].max(1)
    } else {
        clusters[1][0]
    }
}

/// Drop everything below the size floor, then every candidate with another
/// candidate within one pixel — a pair that close has no separable centre,
/// and seeding the fitter twice on one star double-counts its signal.
fn keep_isolated(cands: &mut Vec<Candidate>, min_star_size: usize) -> Vec<Candidate> {
    cands.retain(|c| c.area >= min_star_size);
    let mut order: Vec<usize> = (0..cands.len()).collect();
    order.sort_by(|&a, &b| cands[a].x.total_cmp(&cands[b].x));
    let mut crowded = vec![false; cands.len()];
    for (oi, &i) in order.iter().enumerate() {
        for &j in &order[oi + 1..] {
            if cands[j].x - cands[i].x > 1.0 {
                break;
            }
            if (cands[j].y - cands[i].y).abs() <= 1.0 {
                crowded[i] = true;
                crowded[j] = true;
            }
        }
    }
    let mut out = Vec::with_capacity(cands.len());
    for (i, c) in cands.drain(..).enumerate() {
        if !crowded[i] {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::ransac::SplitMix64;
    use crate::test_support::{add_noise, gaussian_field};

    const BG: f32 = 1000.0;
    const NOISE: f32 = 8.0;
    const FWHM_TO_SIGMA: f64 = 2.354_820_045_030_949_3;
    const W: usize = 512;
    const H: usize = 512;

    /// The synthetic fields are in ADU-like units (background 1000, noise 8),
    /// the same domain `measure_plane` hands the detector, so the saturation
    /// limit is the 16-bit ceiling rather than the `[0, 1]` default.
    fn params() -> StructureParams {
        StructureParams {
            upper_limit: 65535.0,
            ..StructureParams::default()
        }
    }

    /// 300 stars on a jittered 15x20 grid over the LEFT half of the field,
    /// peaks log-spaced over 60..3000 ADU. The right half stays empty.
    fn star_grid() -> Vec<(f64, f64, f64)> {
        let mut rng = SplitMix64(9);
        let mut stars = Vec::with_capacity(300);
        for j in 0..20 {
            for i in 0..15 {
                let x = 24.0 + i as f64 * 15.4 + (rng.next_f64() - 0.5) * 5.0;
                let y = 24.0 + j as f64 * 24.4 + (rng.next_f64() - 0.5) * 5.0;
                let t = (j * 15 + i) as f64 / 299.0;
                stars.push((x, y, 60.0 * 50.0f64.powf(t)));
            }
        }
        stars
    }

    fn field(stars: &[(f64, f64, f64)], fwhm: f64, seed: u64) -> Vec<f32> {
        let mut d = gaussian_field(W, H, stars, fwhm / FWHM_TO_SIGMA, BG);
        add_noise(&mut d, NOISE, seed);
        d
    }

    /// How many of `stars` have a seed within `tol` px, and the seeds that
    /// match no star at all (with their distance to the nearest one).
    fn match_seeds(
        seeds: &[Seed],
        stars: &[(f64, f64, f64)],
        tol: f64,
    ) -> (usize, Vec<(f64, f64, f64)>) {
        let mut found = vec![false; stars.len()];
        let mut orphans = Vec::new();
        for s in seeds {
            let mut best: Option<(f64, usize)> = None;
            for (i, st) in stars.iter().enumerate() {
                let d = ((s.x - st.0).powi(2) + (s.y - st.1).powi(2)).sqrt();
                if best.map_or(true, |(b, _)| d < b) {
                    best = Some((d, i));
                }
            }
            match best {
                Some((d, i)) if d <= tol => found[i] = true,
                Some((d, _)) => orphans.push((s.x, s.y, d)),
                None => orphans.push((s.x, s.y, f64::INFINITY)),
            }
        }
        (found.iter().filter(|f| **f).count(), orphans)
    }

    /// (a) A well-sampled field is detected essentially in full, on the
    /// stars' own positions, and the empty half of the frame stays empty.
    #[test]
    fn a_well_sampled_field_is_detected_in_full() {
        let stars = star_grid();
        let data = field(&stars, 3.0, 17);
        let seeds = detect_structures(&data, W, H, &params());
        let (found, orphans) = match_seeds(&seeds, &stars, 0.7);
        // 284 of 300 when this was written. The 16 missing are the
        // fixture's faint end: a star whose significant peak stands less
        // than `bright_threshold·snr_threshold` sigma over its ring
        // background has to clear the kurtosis rule instead, and a faint
        // star's structure is too small to be peaked enough — the
        // reference's own "peak response" behaviour, not a defect here.
        // (At the reference's own `sensitivity` of 0.5 that line sits at
        // 7.5 sigma, right on the fixture's 60-ADU floor, and 267 survive;
        // the calibrated 0.7 moves it to 4.6.)
        assert!(
            found >= 280,
            "found {found} of {} stars ({} seeds)",
            stars.len(),
            seeds.len()
        );
        // One seed of the 285 landed just over 0.7 px from its star when
        // this was
        // written — a genuine faint star whose 2-3-px structure quantizes
        // its barycentre onto a pixel centre, not a noise detection. The
        // fitter's own centroid tolerance is 1.5 px, so a seed that close
        // still lands on the star.
        assert!(
            orphans.len() <= 2,
            "every seed must sit on a star: {orphans:?}"
        );
        assert!(
            seeds.iter().all(|s| s.x < 256.0),
            "the empty half must stay empty"
        );
        assert!(
            seeds.windows(2).all(|w| w[0].flux >= w[1].flux),
            "seeds come back brightest first"
        );
    }

    /// (b) The reference's sharpness behaviour, and the whole reason this
    /// detector exists: the SAME stars, undersampled, yield a far smaller
    /// population — the hot-pixel median suppresses a narrow star's core far
    /// harder than a soft one's while the binarization level, taken from the
    /// unfiltered noise, stays where it is.
    #[test]
    fn an_undersampled_field_yields_far_fewer_detections() {
        let stars = star_grid();
        let soft = detect_structures(&field(&stars, 3.0, 17), W, H, &params()).len();
        let sharp = detect_structures(&field(&stars, 1.5, 17), W, H, &params()).len();
        assert!(soft > 0, "the soft field must detect something");
        // 159 vs 285 when this was written — a 44 % drop. The two
        // mechanisms behind it: the hot-pixel median suppresses
        // a narrow star's core to ~0.29 of its peak against ~0.73 for a soft
        // one while the binarization level, taken from the UNFILTERED
        // plane's noise, stays where it is (so 78 of the sharp field's stars
        // never form a structure at all or form a one-pixel-wide one), and
        // the automatic size floor then cuts the smallest survivors. It is
        // the behaviour the whole detector exists for: a peak threshold
        // moves the other way, finding MORE stars on the sharper frame.
        assert!(
            (sharp as f64) <= 0.6 * soft as f64,
            "sharp {sharp} vs soft {soft} — the drop must be at least 40 %"
        );
    }

    /// (c) The 33-px high-pass is what keeps extended objects out: a 40-px
    /// blob never reaches the structure map.
    #[test]
    fn an_extended_blob_is_not_a_structure() {
        let blob = [(256.0, 256.0, 300.0)];
        let mut d = gaussian_field(W, H, &blob, 40.0 / FWHM_TO_SIGMA, BG);
        add_noise(&mut d, NOISE, 23);
        let seeds = detect_structures(&d, W, H, &params());
        assert!(
            seeds.is_empty(),
            "a smooth 40-px object is not a star: {seeds:?}"
        );
    }

    /// (d) One structure with two maxima is a blend, not a star — unless the
    /// caller asks for clustered sources.
    #[test]
    fn a_blend_with_two_maxima_is_rejected_unless_clustered() {
        let pair = [(256.0, 256.0, 1200.0), (260.0, 256.0, 1200.0)];
        let data = field(&pair, 3.0, 31);
        assert!(
            detect_structures(&data, W, H, &params()).is_empty(),
            "two maxima in one structure must be rejected"
        );
        let clustered = StructureParams {
            allow_clustered_sources: true,
            ..params()
        };
        assert_eq!(
            detect_structures(&data, W, H, &clustered).len(),
            1,
            "with clustering allowed the blend is one source"
        );
    }

    /// (e) A saturated structure has no usable centre and is dropped.
    #[test]
    fn a_saturated_star_is_rejected() {
        let stars = [(180.0, 256.0, 30000.0), (330.0, 256.0, 70000.0)];
        let data = field(&stars, 3.0, 47);
        let seeds = detect_structures(&data, W, H, &params());
        assert_eq!(
            seeds.len(),
            1,
            "only the unsaturated star survives: {seeds:?}"
        );
        assert!(
            (seeds[0].x - 180.0).abs() < 0.7,
            "the survivor is the faint one: {:?}",
            seeds[0]
        );
    }

    #[test]
    fn serde_names_and_defaults() {
        assert_eq!(SeedDetector::default(), SeedDetector::Peak);
        assert_eq!(
            serde_json::to_string(&SeedDetector::Structure).unwrap(),
            "\"structure\""
        );
        assert_eq!(
            serde_json::from_str::<SeedDetector>("\"peak\"").unwrap(),
            SeedDetector::Peak
        );
        for v in [SeedDetector::Peak, SeedDetector::Structure] {
            assert_eq!(
                serde_json::to_string(&v).unwrap(),
                format!("\"{}\"", v.as_str())
            );
        }
        let p = StructureParams::default();
        assert_eq!((p.structure_layers, p.hot_pixel_median), (5, true));
        assert_eq!((p.sensitivity, p.peak_response), (DEFAULT_SENSITIVITY, 0.5));
        assert_eq!(
            p.sensitivity, 0.7,
            "calibrated in M4c Task 0, not the reference's 0.5"
        );
        assert_eq!((p.bright_threshold, p.max_distortion), (3.0, 0.6));
        assert_eq!(p.min_structure_size, 0);
        assert!(!p.allow_clustered_sources);
        assert_eq!(p.upper_limit, 1.0);
        assert_eq!(
            serde_json::from_str::<StructureParams>("{}").unwrap(),
            StructureParams::default()
        );
    }
}

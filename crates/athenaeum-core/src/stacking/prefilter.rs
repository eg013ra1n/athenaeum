//! Pre-filters applied to the image the measurement's star DETECTION runs
//! on — never to the image the PSF fits, the aperture flux, the background
//! model or the noise estimate see (math reference §5.1: the reference
//! detector's structure map begins with an optional 3×3 median, "hot-pixel
//! filter radius 1", and everything downstream of detection still measures
//! the untouched pixels).
//!
//! Why this is a lever the detection threshold is not: a 3×3 median cuts a
//! star's peak by a factor that depends on how wide the star is. For a
//! Gaussian of σ px the four edge neighbours sit at `exp(−1/2σ²)` of the
//! peak and the four diagonals at `exp(−1/σ²)`, so the median of the nine
//! is the edge value — ≈ 0.31·peak at FWHM 1.55 px (σ 0.66) but ≈ 0.79·peak
//! at FWHM 3.4 px (σ 1.44). An undersampled frame's stars are therefore
//! suppressed ~2.5× more than a soft frame's, which is exactly the
//! sharpness-dependent factor a single `background + k·noise` threshold
//! cannot produce (M4a Task 2 fix round 1, ruling R-M4a-13).

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// What the seed detection runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum SeedPrefilter {
    /// Detect on the plane as it is.
    #[default]
    None,
    /// Detect on the 3×3 median of the plane (math reference §5.1).
    Median3,
}

/// 3×3 median of `src` (`w × h`, row-major), returned as a new buffer.
///
/// Border pixels use the neighbours that exist — a 2×2 window at a corner, a
/// 2×3 along an edge. Nothing is wrapped and nothing is padded with zero:
/// either would invent a dark neighbour and pull an edge star's median down
/// far harder than an interior star's, which is the one thing this filter
/// must not do (its whole purpose is that the suppression depends on the
/// star, not on where it sits).
///
/// With an even window the upper median is taken (element `k/2` of the
/// sorted `k`), so a 4-pixel corner reports its third-smallest. Non-finite
/// values are ordered by `f32::total_cmp` rather than filtered out; a lone
/// NaN neighbour sorts to the top and cannot displace the median of a
/// full 9-pixel window.
///
/// One rayon task per output row; the window lives in a stack array, so
/// there is no per-pixel allocation.
pub fn median3(src: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h];
    if w == 0 || h == 0 || src.len() < w * h {
        return out;
    }
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let y0 = y.saturating_sub(1);
        let y1 = (y + 1).min(h - 1);
        for (x, o) in row.iter_mut().enumerate() {
            let x0 = x.saturating_sub(1);
            let x1 = (x + 1).min(w - 1);
            let mut win = [0.0f32; 9];
            let mut k = 0usize;
            for yy in y0..=y1 {
                let base = yy * w;
                for xx in x0..=x1 {
                    win[k] = src[base + xx];
                    k += 1;
                }
            }
            let (_, med, _) = win[..k].select_nth_unstable_by(k / 2, f32::total_cmp);
            *o = *med;
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the filter: a narrow star loses far more of its
    /// peak than a wide one, on the same pixel grid.
    #[test]
    fn a_sharp_star_loses_more_peak_than_a_soft_one() {
        let (w, h) = (41usize, 41usize);
        let peak_after = |sigma: f64| {
            let mut d = vec![0.0f32; w * h];
            for y in 0..h {
                for x in 0..w {
                    let (dx, dy) = (x as f64 - 20.0, y as f64 - 20.0);
                    d[y * w + x] = (-(dx * dx + dy * dy) / (2.0 * sigma * sigma)).exp() as f32;
                }
            }
            median3(&d, w, h)[20 * w + 20] as f64
        };
        // FWHM 1.55 px and 3.4 px — the two nights of the acceptance set.
        let sharp = peak_after(1.55 / 2.354_820_045_030_949_3);
        let soft = peak_after(3.4 / 2.354_820_045_030_949_3);
        assert!(
            (sharp - 0.31).abs() < 0.02,
            "sharp star keeps {sharp} of its peak"
        );
        assert!(
            (soft - 0.79).abs() < 0.02,
            "soft star keeps {soft} of its peak"
        );
        assert!(
            soft / sharp > 2.3,
            "the suppression must depend on width: {}",
            soft / sharp
        );
    }

    /// A single hot pixel — the filter's namesake — is removed outright,
    /// and a flat field is unchanged everywhere including the border.
    #[test]
    fn a_hot_pixel_goes_and_a_flat_field_survives_the_border() {
        let (w, h) = (7usize, 5usize);
        let mut d = vec![10.0f32; w * h];
        d[2 * w + 3] = 5000.0;
        let m = median3(&d, w, h);
        assert_eq!(m[2 * w + 3], 10.0, "the hot pixel must not survive");
        assert!(
            m.iter().all(|&v| v == 10.0),
            "a flat field must come back flat, borders included: {m:?}"
        );
    }

    /// The border uses real neighbours only — never a wrapped or zero one.
    /// A bright left column stays bright after filtering; a zero-padded
    /// implementation would darken it.
    #[test]
    fn the_border_never_borrows_a_pixel_that_does_not_exist() {
        let (w, h) = (5usize, 5usize);
        let mut d = vec![100.0f32; w * h];
        for y in 0..h {
            d[y * w] = 900.0;
        }
        let m = median3(&d, w, h);
        // Corner (0,0): window {900, 100, 900, 100} → upper median 900.
        assert_eq!(m[0], 900.0, "corner");
        // Edge (0,2): window {900,100,900,100,900,100} → upper median 900.
        assert_eq!(m[2 * w], 900.0, "left edge");
        // Interior (1,2): window has two 900s and seven 100s → 100.
        assert_eq!(m[2 * w + 1], 100.0, "interior next to the bright column");
    }

    #[test]
    fn serde_names_and_default() {
        assert_eq!(SeedPrefilter::default(), SeedPrefilter::None);
        assert_eq!(
            serde_json::to_string(&SeedPrefilter::Median3).unwrap(),
            "\"median3\""
        );
        assert_eq!(
            serde_json::from_str::<SeedPrefilter>("\"none\"").unwrap(),
            SeedPrefilter::None
        );
        assert!(median3(&[], 0, 0).is_empty());
    }
}

//! Hot-pixel cosmetic correction: where the hot pixels are ([`HotPixelMap`],
//! measured once per master dark) and what to put in their place
//! ([`apply_hot_pixel_correction`], run on a calibrated frame before any
//! debayering).
//!
//! **Detection lives in the dark, not in the light.** A hot pixel is a sensor
//! defect: it reads high with no light on it, which is exactly what a master
//! dark measures. Flagging outliers in the light itself cannot tell a hot
//! pixel from a faint star, so v1 maps only from the dark — a frame with no
//! dark linked simply gets no correction, honestly skipped by the caller.
//!
//! The threshold is `median + HOT_SIGMA · 1.4826 · MAD` over the dark's whole
//! plane. Median and MAD (rather than mean and standard deviation) because the
//! hot pixels are themselves in the sample and would inflate a
//! non-robust spread until they no longer stood out; `1.4826 · MAD` is the
//! standard-deviation estimate of a normal distribution, so `HOT_SIGMA` reads
//! as sigmas.
//!
//! **Two refusals guard that threshold, because over-flagging is destructive
//! and skipping is not.** Every mapped pixel gets median-filtered away, so a
//! map that is wrong in the direction of "too many" silently smooths real
//! signal out of the frame; a map that is empty merely leaves the frame as it
//! was. So: a MAD of exactly zero yields no map at all (it means over half the
//! plane reads the same value — a heavily stacked integer-BITPIX master dark
//! whose read noise quantizes to whole ADU does this routinely, and there is
//! no spread left to measure sigmas against), and a map flagging more than
//! [`MAX_HOT_FRACTION`] of the plane is thrown away whole. Neither case can be
//! rescued by clamping the threshold to something small: `median + ε` on a
//! quantized dark flags every pixel one ADU above the median, which is a large
//! fraction of the sensor.
//!
//! **Replacement is a neighbourhood median, and the neighbourhood is
//! colour-aware.** On a mono frame the eight pixels of the 3x3 window measure
//! the same thing as the centre. On a mosaic they do not: four of them carry a
//! different colour, so their median would drag the pixel toward the frame's
//! other channels and leave a coloured speck where a hot pixel used to be.
//! Stepping by two lands on the eight same-colour cells of the 5x5 window
//! instead — a 2x2 mosaic cell repeats with period two on both axes, so a
//! stride-2 neighbour is always the centre's own colour, whatever the pattern
//! and whatever the `XBAYROFF`/`YBAYROFF` phase.

use std::path::Path;

use crate::integration::banded::{band_rows_for_budget, BandSource};
use crate::integration::cfa::{cfa_channel_at, CfaGeometry};
use crate::integration::engine::BAND_BUDGET_BYTES;
use crate::integration::IntegrationError;

/// How many robust sigmas above the dark's median a pixel must read to count
/// as hot. Fixed in v1 — the user-facing control is the on/off toggle, not
/// this number.
pub const HOT_SIGMA: f64 = 10.0;

/// Consistency constant turning a median absolute deviation into the standard
/// deviation of a normal distribution.
const MAD_TO_SIGMA: f64 = 1.4826;

/// Largest fraction of the plane a map may flag before it is refused whole.
///
/// A real sensor's hot pixels are a small defect population — a fraction of a
/// percent. A map covering a large slice of the frame is not a hot-pixel map,
/// it is a sign the statistics were degenerate, and applying it would median-
/// filter that whole slice of real signal away. 5 % is far above any plausible
/// defect count and far below the damage threshold.
const MAX_HOT_FRACTION: f64 = 0.05;

/// Which pixels of one master dark read hot, as row-major indices into a
/// `width * height` plane, ascending.
///
/// Built by [`hot_pixel_map_from_dark`] and valid only for frames of that same
/// geometry — [`apply_hot_pixel_correction`] refuses anything else rather than
/// correcting at the wrong coordinates.
#[derive(Debug, Clone)]
pub struct HotPixelMap {
    width: usize,
    height: usize,
    /// Row-major pixel indices, ascending. `u32` bounds the map at ~4.29 G
    /// pixels, checked at build time.
    indices: Vec<u32>,
}

impl HotPixelMap {
    /// How many pixels the map flags.
    pub fn len(&self) -> usize {
        self.indices.len()
    }

    /// Whether the map flags nothing — the dark showed no outlier, so applying
    /// it is a no-op.
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }
}

/// Read a master dark's full plane and flag every pixel reading above
/// `median + HOT_SIGMA · 1.4826 · MAD`.
///
/// Returns an EMPTY map — never an error — for the three degenerate cases a
/// dark can present: a MAD of zero, a non-finite threshold, and a result over
/// [`MAX_HOT_FRACTION`] of the plane. All three are logged; see the module docs
/// for why refusing beats correcting on a threshold that cannot be trusted.
///
/// The plane is band-streamed in (bounded working set) but the statistics need
/// the whole distribution, so one plane plus one scratch copy is the memory
/// cost. Callers cache the result per dark path: the map depends on the dark
/// alone, never on the light it will be applied to.
pub fn hot_pixel_map_from_dark(
    dark_path: &Path,
    scratch_dir: &Path,
) -> Result<HotPixelMap, IntegrationError> {
    let (width, height, data) = read_full_plane(dark_path, scratch_dir)?;
    if width.saturating_mul(height) > u32::MAX as usize {
        return Err(IntegrationError::BadInput(format!(
            "master dark {width}x{height} exceeds the {} pixels a hot-pixel map can index ({})",
            u32::MAX,
            dark_path.display()
        )));
    }

    // Sort a copy for the median, then rewrite that same copy into absolute
    // deviations and sort again for the MAD — two sorts, but only one extra
    // plane of memory instead of two. `total_cmp` gives a total order, so a
    // NaN-carrying dark sorts deterministically instead of leaving the buffer
    // in an arbitrary state.
    let mut work: Vec<f32> = data.clone();
    work.sort_unstable_by(|a, b| a.total_cmp(b));
    let median = median_of_sorted(&work);
    for v in work.iter_mut() {
        *v = (*v as f64 - median).abs() as f32;
    }
    work.sort_unstable_by(|a, b| a.total_cmp(b));
    let mad = median_of_sorted(&work);
    drop(work);

    let empty = || HotPixelMap {
        width,
        height,
        indices: Vec::new(),
    };

    if mad == 0.0 {
        // More than half the plane reads exactly the median — the signature of
        // a stacked integer-BITPIX master dark whose noise quantizes to whole
        // ADU, not of a broken file. There is no spread to measure sigmas
        // against, and any small substitute threshold would flag every pixel
        // one ADU high, i.e. a large fraction of the sensor. No map.
        tracing::debug!(
            path = %dark_path.display(),
            median,
            "zero mad in master dark — no pixels mapped"
        );
        return Ok(empty());
    }

    let threshold = median + HOT_SIGMA * MAD_TO_SIGMA * mad;
    if !threshold.is_finite() {
        // Reachable only on a dark whose median is NaN or infinite (a broken
        // or mostly-NaN file). Every comparison against it would be false, so
        // the map would silently come back empty; say so instead.
        tracing::warn!(
            path = %dark_path.display(),
            median,
            mad,
            threshold,
            "hot-pixel threshold not finite — no pixels mapped"
        );
        return Ok(empty());
    }

    // Ascending by construction: a row-major scan visits indices in order.
    let indices: Vec<u32> = data
        .iter()
        .enumerate()
        .filter(|(_, &v)| (v as f64) > threshold)
        .map(|(i, _)| i as u32)
        .collect();

    let total = width * height;
    if indices.len() as f64 > MAX_HOT_FRACTION * total as f64 {
        // Not a defect population. Whatever produced this — a bimodal dark, a
        // gradient, a file that is not really a dark — applying it would
        // median-filter that share of every frame. Refusing costs the run its
        // cosmetic correction; applying it would cost real signal.
        tracing::warn!(
            path = %dark_path.display(),
            count = indices.len(),
            total,
            "hot-pixel map exceeds the safety cap — no pixels mapped"
        );
        return Ok(empty());
    }

    tracing::debug!(
        path = %dark_path.display(),
        width,
        height,
        median,
        mad,
        threshold,
        count = indices.len(),
        "hot-pixel map built"
    );
    Ok(HotPixelMap {
        width,
        height,
        indices,
    })
}

/// Replace every mapped pixel of `data` with the median of its neighbours,
/// returning how many were replaced.
///
/// The neighbourhood is the eight pixels of the 3x3 window for a mono frame,
/// and the eight same-colour cells at stride 2 when `cfa` declares a mosaic —
/// see the module docs for why the stride is the whole of the colour
/// awareness. Border pixels use whatever subset of that neighbourhood is in
/// bounds; a centre with no in-bounds neighbour at all (only reachable on a
/// degenerate frame) is left untouched and not counted.
///
/// Replacement is sequential and in place, so a hot pixel already corrected
/// earlier in the scan contributes its repaired value to a later neighbour
/// rather than its defect — which is what a cluster of adjacent hot pixels
/// needs. A geometry that does not match the map's is refused wholesale
/// (returns 0, warns): correcting at the wrong coordinates would damage good
/// pixels.
pub fn apply_hot_pixel_correction(
    data: &mut [f32],
    width: usize,
    height: usize,
    map: &HotPixelMap,
    cfa: Option<CfaGeometry>,
) -> u64 {
    if map.width != width || map.height != height || data.len() < width * height {
        tracing::warn!(
            map_width = map.width,
            map_height = map.height,
            width,
            height,
            len = data.len(),
            "hot-pixel map geometry mismatch — correction skipped"
        );
        return 0;
    }

    let stride = if cfa.is_some() { 2isize } else { 1 };
    let mut neighbours = [0f32; 8];
    let mut replaced = 0u64;

    for &raw in &map.indices {
        let idx = raw as usize;
        debug_assert!(
            idx < width * height,
            "map index {idx} outside {width}x{height}"
        );
        let (x, y) = (idx % width, idx / width);

        let mut n = 0usize;
        for dy in [-stride, 0, stride] {
            for dx in [-stride, 0, stride] {
                if dx == 0 && dy == 0 {
                    continue;
                }
                let (nx, ny) = (x as isize + dx, y as isize + dy);
                if nx < 0 || ny < 0 || nx >= width as isize || ny >= height as isize {
                    continue;
                }
                let (nx, ny) = (nx as usize, ny as usize);
                // The stride IS the channel selection; this pins that claim so
                // a future change to the offsets cannot quietly break it.
                debug_assert!(
                    cfa.is_none_or(|g| cfa_channel_at(nx, ny, g) == cfa_channel_at(x, y, g)),
                    "stride-{stride} neighbour ({nx},{ny}) left the CFA channel of ({x},{y})"
                );
                neighbours[n] = data[ny * width + nx];
                n += 1;
            }
        }
        if n == 0 {
            continue;
        }

        let window = &mut neighbours[..n];
        window.sort_unstable_by(|a, b| a.total_cmp(b));
        data[idx] = median_of_sorted(window) as f32;
        replaced += 1;
    }

    replaced
}

/// Median of an ascending slice: the middle element for an odd count, the mean
/// of the two middle ones for an even count. Empty slice → 0.0.
fn median_of_sorted(sorted: &[f32]) -> f64 {
    let n = sorted.len();
    if n == 0 {
        return 0.0;
    }
    if n % 2 == 1 {
        sorted[n / 2] as f64
    } else {
        (sorted[n / 2 - 1] as f64 + sorted[n / 2] as f64) / 2.0
    }
}

/// Band-read an entire single-plane frame into one `Vec<f32>`: bounded working
/// set on the read (one row-band at a time), one full plane held at the end
/// because the statistics need the whole distribution.
fn read_full_plane(
    path: &Path,
    scratch_dir: &Path,
) -> Result<(usize, usize, Vec<f32>), IntegrationError> {
    let mut src = BandSource::open(&[path.to_path_buf()], scratch_dir)?;
    let (w, h) = (src.width(), src.height());
    let band_rows = band_rows_for_budget(w, 1, BAND_BUDGET_BYTES).min(h);
    let mut data = vec![0f32; w * h];
    let mut bufs = vec![Vec::new()];
    let mut y = 0;
    while y < h {
        let rows = band_rows.min(h - y);
        src.read_band(y, rows, &mut bufs)?;
        data[y * w..(y + rows) * w].copy_from_slice(&bufs[0]);
        y += rows;
    }
    Ok((w, h, data))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::keywords::Bayer;
    use crate::fits_writer::write_fits_f32;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn write_plane(dir: &Path, name: &str, w: usize, h: usize, data: &[f32]) -> PathBuf {
        let p = dir.join(name);
        write_fits_f32(&p, w, h, 1, data, &[]).unwrap();
        p
    }

    fn rggb() -> CfaGeometry {
        CfaGeometry {
            pattern: Bayer::Rggb,
            xoff: 0,
            yoff: 0,
        }
    }

    /// Painted from the RGGB definition directly, NOT from `cfa_channel_at`:
    /// even/even is R, odd/odd is B, the anti-diagonal is G.
    fn rggb_mosaic(w: usize, h: usize) -> Vec<f32> {
        let mut data = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                data[y * w + x] = match (x % 2 == 0, y % 2 == 0) {
                    (true, true) => 0.8,   // R
                    (false, false) => 0.2, // B
                    _ => 0.5,              // G
                };
            }
        }
        data
    }

    /// A dark with a real spread maps exactly the pixels above
    /// `median + HOT_SIGMA · 1.4826 · MAD`, at the right indices.
    ///
    /// The fixture is built so the arithmetic is pinned from BOTH sides rather
    /// than merely separating an obvious spike from an obvious background.
    /// 125 pixels alternate 300.0/302.0 (63 low, 62 high), then three probes:
    ///
    /// - sorted, elements 63 and 64 are both 302.0 → `median = 302.0`;
    /// - the deviations are 63 twos, 62 zeros and the three probes, whose
    ///   elements 63 and 64 are both 2.0 → `MAD = 2.0`;
    /// - so `threshold = 302 + 10 · 1.4826 · 2 = 331.652`.
    ///
    /// Probe 327.0 sits below that and must NOT be flagged — it is above
    /// `median + 10 · MAD` (322), so dropping the 1.4826 factor flags it and
    /// fails the test. Probe 336.0 sits just above and MUST be flagged — a
    /// sigma of 12 would put the threshold at 337.6 and miss it. The threshold
    /// is therefore bracketed to (327, 336].
    #[test]
    fn map_flags_exactly_the_spikes() {
        let dir = tempdir().unwrap();
        let (w, h) = (16usize, 8usize);
        let mut data: Vec<f32> = (0..125)
            .map(|i| if i % 2 == 0 { 300.0 } else { 302.0 })
            .collect();
        data.push(327.0); // idx 125 — below the threshold
        data.push(336.0); // idx 126 — above it
        data.push(5000.0); // idx 127 — the obvious spike
        assert_eq!(data.len(), w * h);
        let path = write_plane(dir.path(), "dark.fits", w, h, &data);

        let map = hot_pixel_map_from_dark(&path, dir.path()).unwrap();
        assert!(!map.is_empty());
        assert_eq!(map.len(), 2);
        assert_eq!(map.indices, vec![126u32, 127u32]);
    }

    /// A dark whose background is a single repeated value has MAD 0 even with
    /// obvious spikes in it — over half the plane equals the median. That is
    /// what a stacked integer-BITPIX master dark looks like, so the answer is
    /// no map rather than a substitute threshold that would flag every pixel
    /// one step above the median. The spikes go uncorrected; nothing is
    /// damaged.
    #[test]
    fn flat_background_with_spikes_is_refused() {
        let dir = tempdir().unwrap();
        let (w, h) = (16usize, 8usize);
        let mut data = vec![300.0f32; w * h];
        data[5] = 5000.0;
        data[100] = 5000.0;
        let path = write_plane(dir.path(), "flat_bg.fits", w, h, &data);

        let map = hot_pixel_map_from_dark(&path, dir.path()).unwrap();
        assert!(map.is_empty(), "zero MAD must not produce a map");
    }

    /// A map covering more than 5 % of the plane is not a defect population.
    /// Applying it would median-filter that share of every frame away, so it is
    /// refused whole — even though the statistics here are perfectly
    /// well-formed (median 302.0, MAD 2.0, threshold 331.652) and the 20
    /// flagged pixels really do clear it.
    #[test]
    fn over_flagged_map_is_refused() {
        let dir = tempdir().unwrap();
        let (w, h) = (16usize, 8usize);
        let mut data: Vec<f32> = (0..108)
            .map(|i| if i % 2 == 0 { 300.0 } else { 302.0 })
            .collect();
        data.extend(std::iter::repeat_n(5000.0f32, 20));
        assert_eq!(data.len(), w * h);
        assert!(
            20.0 > MAX_HOT_FRACTION * (w * h) as f64,
            "fixture must trip the cap"
        );
        let path = write_plane(dir.path(), "over_flagged.fits", w, h, &data);

        let map = hot_pixel_map_from_dark(&path, dir.path()).unwrap();
        assert!(map.is_empty(), "a map over the safety cap must be refused");
    }

    /// Mono replacement is the median of the eight 3x3 neighbours, and it
    /// touches nothing but the mapped pixel.
    #[test]
    fn mono_replacement_uses_3x3_median() {
        let (w, h) = (9usize, 9usize);
        let mut data = vec![1.0f32; w * h];
        let idx = 4 * w + 4;
        data[idx] = 9.0;
        let map = HotPixelMap {
            width: w,
            height: h,
            indices: vec![idx as u32],
        };

        let replaced = apply_hot_pixel_correction(&mut data, w, h, &map, None);
        assert_eq!(replaced, 1);
        assert_eq!(data[idx], 1.0);
        assert!(data.iter().all(|&v| v == 1.0), "no other pixel touched");

        // Eight distinct neighbours pin the even-count median — the two middle
        // values of 1..=8 are 4 and 5, so any off-by-one in the index
        // arithmetic lands somewhere other than 4.5.
        let mut data = vec![0f32; w * h];
        let mut next = 1.0f32;
        for dy in [-1isize, 0, 1] {
            for dx in [-1isize, 0, 1] {
                let (nx, ny) = ((4 + dx) as usize, (4 + dy) as usize);
                data[ny * w + nx] = if dx == 0 && dy == 0 {
                    9.0
                } else {
                    let v = next;
                    next += 1.0;
                    v
                };
            }
        }
        let replaced = apply_hot_pixel_correction(&mut data, w, h, &map, None);
        assert_eq!(replaced, 1);
        assert_eq!(data[idx], 4.5, "mean of the two middle neighbours");
    }

    /// A mosaic pixel is rebuilt from its own colour only: the stride-2
    /// neighbourhood never reaches a G or B site from an R centre.
    #[test]
    fn cfa_replacement_stays_in_channel() {
        let (w, h) = (8usize, 8usize);
        let mut data = rggb_mosaic(w, h);
        let r_idx = 4 * w + 4; // even/even -> R
        let g_idx = 4 * w + 5; // odd/even  -> G
        data[r_idx] = 9.0;
        data[g_idx] = 9.0;
        let map = HotPixelMap {
            width: w,
            height: h,
            indices: vec![r_idx as u32, g_idx as u32],
        };

        let replaced = apply_hot_pixel_correction(&mut data, w, h, &map, Some(rggb()));
        assert_eq!(replaced, 2);
        assert_eq!(data[r_idx], 0.8, "R site rebuilt from R neighbours only");
        assert_eq!(data[g_idx], 0.5, "G site rebuilt from G neighbours only");
    }

    /// Border pixels fall back to the in-bounds subset of the neighbourhood,
    /// mono and CFA alike; a centre with no in-bounds neighbour at all is left
    /// untouched and uncounted rather than producing a NaN.
    #[test]
    fn border_pixels_use_the_in_bounds_subset() {
        // Mono corner (0,0): in-bounds neighbours are (1,0), (0,1), (1,1).
        let (w, h) = (5usize, 5usize);
        let mut data = vec![1.0f32; w * h];
        data[0] = 9.0;
        data[1] = 2.0;
        data[w] = 4.0;
        data[w + 1] = 6.0;
        let map = HotPixelMap {
            width: w,
            height: h,
            indices: vec![0],
        };
        let replaced = apply_hot_pixel_correction(&mut data, w, h, &map, None);
        assert_eq!(replaced, 1);
        assert_eq!(data[0], 4.0, "median of the three in-bounds neighbours");

        // CFA corner (1,1): stride-2 neighbours are (1,3), (3,1), (3,3).
        let (cw, ch) = (8usize, 8usize);
        let mut mosaic = rggb_mosaic(cw, ch);
        let idx = cw + 1; // odd/odd -> B
        mosaic[idx] = 9.0;
        let map = HotPixelMap {
            width: cw,
            height: ch,
            indices: vec![idx as u32],
        };
        let replaced = apply_hot_pixel_correction(&mut mosaic, cw, ch, &map, Some(rggb()));
        assert_eq!(replaced, 1);
        assert_eq!(mosaic[idx], 0.2, "B site rebuilt from B neighbours only");

        // No neighbour in bounds at all: left alone, not counted.
        let mut lone = vec![9.0f32];
        let map = HotPixelMap {
            width: 1,
            height: 1,
            indices: vec![0],
        };
        assert_eq!(apply_hot_pixel_correction(&mut lone, 1, 1, &map, None), 0);
        assert_eq!(lone[0], 9.0);
    }

    /// A map built for a different geometry is refused wholesale — correcting
    /// at the wrong coordinates would corrupt good pixels.
    #[test]
    fn geometry_mismatch_is_a_no_op() {
        let mut data = vec![9.0f32; 16];
        let map = HotPixelMap {
            width: 8,
            height: 8,
            indices: vec![0],
        };
        assert_eq!(apply_hot_pixel_correction(&mut data, 4, 4, &map, None), 0);
        assert!(data.iter().all(|&v| v == 9.0));
    }

    /// The simplest MAD-0 case: every pixel identical, so there is no spread
    /// to measure sigmas against and no map to build.
    #[test]
    fn uniform_dark_flags_nothing() {
        let dir = tempdir().unwrap();
        let (w, h) = (16usize, 8usize);
        let data = vec![300.0f32; w * h];
        let path = write_plane(dir.path(), "uniform.fits", w, h, &data);

        let map = hot_pixel_map_from_dark(&path, dir.path()).unwrap();
        assert!(map.is_empty());
        assert_eq!(map.len(), 0);
    }
}

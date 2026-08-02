//! CFA (colour filter array) channel geometry and per-channel statistics.
//!
//! A one-shot-colour sensor writes a mosaic: every pixel carries exactly one
//! of R/G/B, tiled in a 2x2 cell whose phase is declared by `BAYERPAT` plus
//! the `XBAYROFF`/`YBAYROFF` offsets. Anything that measures a mosaic's level
//! per colour — flat normalization above all — has to know which pixel is
//! which, or the measurement mixes the three channels together and the
//! resulting scale factor tints the frame.
//!
//! This module owns that mapping and nothing else: no header parsing, no I/O.

use crate::fits_writer::keywords::Bayer;

/// One colour of the mosaic. `as usize` / [`CfaChannel::idx`] is the index
/// into the `[R, G, B]` arrays this module returns.
#[repr(usize)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CfaChannel {
    R = 0,
    G = 1,
    B = 2,
}

impl CfaChannel {
    /// Index into an `[R, G, B]` array. Same value as `self as usize`.
    pub const fn idx(self) -> usize {
        self as usize
    }
}

/// The mosaic phase of an image: the declared pattern plus its CFA offsets.
///
/// `xoff`/`yoff` come straight from `XBAYROFF`/`YBAYROFF` and are applied as
/// a shift of the pattern relative to pixel (0, 0). They are signed and
/// unbounded on the wire, so the mapping folds them modulo 2.
///
/// **Row order.** `BAYERPAT` is treated here as describing the mosaic in FILE
/// row order — the dominant capture-software convention. A declared
/// `ROWORDER` changes display orientation, not the layout this math walks, so
/// it is deliberately not folded into the geometry. This is the one error
/// class that does not cancel: a vertical flip is a one-row phase shift, and
/// a one-row shift of RGGB is GBRG — a different family, not a relabelling of
/// the same one. If a writer declares `BAYERPAT` in display orientation
/// instead, the R and B classes mix and per-channel statistics degrade toward
/// the global-scalar answer; never worse than the global normalization that
/// preceded them, but no longer per-channel. Should folding a row flip in
/// ever become necessary, the exact correction is `yoff += h - 1`
/// (`rem_euclid` absorbs the magnitude: +1 of phase for even heights, 0 for
/// odd) — `h - 1 - y` and `h - 1 + y` are congruent modulo 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CfaGeometry {
    pub pattern: Bayer,
    pub xoff: i64,
    pub yoff: i64,
}

/// Which colour pixel `(x, y)` carries under geometry `g`.
pub fn cfa_channel_at(x: usize, y: usize, g: CfaGeometry) -> CfaChannel {
    // 2x2 cell grids, row-major (row 0 = top row of the pattern string).
    const RGGB: [[CfaChannel; 2]; 2] = [
        [CfaChannel::R, CfaChannel::G],
        [CfaChannel::G, CfaChannel::B],
    ];
    const BGGR: [[CfaChannel; 2]; 2] = [
        [CfaChannel::B, CfaChannel::G],
        [CfaChannel::G, CfaChannel::R],
    ];
    const GBRG: [[CfaChannel; 2]; 2] = [
        [CfaChannel::G, CfaChannel::B],
        [CfaChannel::R, CfaChannel::G],
    ];
    const GRBG: [[CfaChannel; 2]; 2] = [
        [CfaChannel::G, CfaChannel::R],
        [CfaChannel::B, CfaChannel::G],
    ];
    let grid = match g.pattern {
        Bayer::Rggb => RGGB,
        Bayer::Bggr => BGGR,
        Bayer::Gbrg => GBRG,
        Bayer::Grbg => GRBG,
    };
    // `rem_euclid`, not `%`: a negative offset must fold to the same phase as
    // its positive twin, and truncating remainder would index out of bounds.
    let row = ((y as i64 + g.yoff).rem_euclid(2)) as usize;
    let col = ((x as i64 + g.xoff).rem_euclid(2)) as usize;
    grid[row][col]
}

/// Per-channel means over the central-third window: `[R, G, B]`.
///
/// The window is the one [`super::engine::central_third_mean`] uses — same
/// integer division, so for any image at least 3 pixels on a side a mono mean
/// and these channel means describe exactly the same pixels.
///
/// A channel with no pixel in the window reports 0.0 rather than a NaN from
/// 0/0. That is reachable whenever the window is smaller than a 2x2 cell (a
/// 3-pixel side gives a 1-pixel window), and it is also what a 1-pixel side
/// yields here: the mono mean widens a collapsed window to one row/column,
/// this one does not, so a 1-pixel-tall image reports `[0.0, 0.0, 0.0]`. No
/// real frame reaches either case; the point of the guard is that a caller
/// dividing by a channel mean gets an obviously-wrong 0, never a NaN that
/// propagates silently into every pixel.
///
/// # Panics
/// If `data` is shorter than `w * h` (row-major, no padding).
pub fn central_third_channel_means(data: &[f32], w: usize, h: usize, g: CfaGeometry) -> [f64; 3] {
    debug_assert!(data.len() >= w * h, "data shorter than {w}x{h}");
    let (x0, x1) = (w / 3, (2 * w) / 3);
    let (y0, y1) = (h / 3, (2 * h) / 3);
    let (mut sum, mut n) = ([0f64; 3], [0u64; 3]);
    for y in y0..y1 {
        for x in x0..x1 {
            let c = cfa_channel_at(x, y, g).idx();
            sum[c] += data[y * w + x] as f64;
            n[c] += 1;
        }
    }
    [0, 1, 2].map(|c| if n[c] == 0 { 0.0 } else { sum[c] / n[c] as f64 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geom(pattern: Bayer, xoff: i64, yoff: i64) -> CfaGeometry {
        CfaGeometry {
            pattern,
            xoff,
            yoff,
        }
    }

    /// The 2x2 cell of every pattern at zero offset, read row-major from the
    /// pattern string (row 0 = the first two letters).
    #[test]
    fn cell_mapping_all_patterns_zero_offset() {
        use CfaChannel::{B, G, R};
        let cases = [
            (Bayer::Rggb, [[R, G], [G, B]]),
            (Bayer::Bggr, [[B, G], [G, R]]),
            (Bayer::Gbrg, [[G, B], [R, G]]),
            (Bayer::Grbg, [[G, R], [B, G]]),
        ];
        for (pattern, cell) in cases {
            let g = geom(pattern, 0, 0);
            for (row, expected_row) in cell.iter().enumerate() {
                for (col, expected) in expected_row.iter().enumerate() {
                    assert_eq!(
                        cfa_channel_at(col, row, g),
                        *expected,
                        "{} at ({col},{row})",
                        pattern.as_str()
                    );
                }
            }
        }
    }

    /// xoff = 1 shifts the pattern one column: the cell of every pattern
    /// becomes the cell of its column-swapped self.
    #[test]
    fn cell_mapping_all_patterns_x_offset_one() {
        use CfaChannel::{B, G, R};
        let cases = [
            (Bayer::Rggb, [[G, R], [B, G]]),
            (Bayer::Bggr, [[G, B], [R, G]]),
            (Bayer::Gbrg, [[B, G], [G, R]]),
            (Bayer::Grbg, [[R, G], [G, B]]),
        ];
        for (pattern, cell) in cases {
            let g = geom(pattern, 1, 0);
            for (row, expected_row) in cell.iter().enumerate() {
                for (col, expected) in expected_row.iter().enumerate() {
                    assert_eq!(
                        cfa_channel_at(col, row, g),
                        *expected,
                        "{} xoff=1 at ({col},{row})",
                        pattern.as_str()
                    );
                }
            }
        }
    }

    /// yoff = 1 shifts the pattern one row, for every pattern.
    #[test]
    fn y_offset_one_shifts_rows() {
        for pattern in [Bayer::Rggb, Bayer::Bggr, Bayer::Gbrg, Bayer::Grbg] {
            let shifted = geom(pattern, 0, 1);
            let plain = geom(pattern, 0, 0);
            for y in 0..4usize {
                for x in 0..4usize {
                    assert_eq!(
                        cfa_channel_at(x, y, shifted),
                        cfa_channel_at(x, y + 1, plain),
                        "{} yoff=1 at ({x},{y})",
                        pattern.as_str()
                    );
                }
            }
        }
    }

    /// The mapping is periodic with period 2 on both axes.
    #[test]
    fn mapping_is_period_two() {
        let g = geom(Bayer::Gbrg, 0, 0);
        for y in 0..6usize {
            for x in 0..6usize {
                assert_eq!(cfa_channel_at(x, y, g), cfa_channel_at(x + 2, y, g));
                assert_eq!(cfa_channel_at(x, y, g), cfa_channel_at(x, y + 2, g));
            }
        }
    }

    /// Negative offsets fold with `rem_euclid`, not with truncating `%`:
    /// -1 must land on the same phase as +1 (and -3, -5, ...).
    #[test]
    fn negative_offsets_fold_like_positive() {
        for pattern in [Bayer::Rggb, Bayer::Bggr, Bayer::Gbrg, Bayer::Grbg] {
            for y in 0..4usize {
                for x in 0..4usize {
                    let plus_one = cfa_channel_at(x, y, geom(pattern, 1, 0));
                    assert_eq!(plus_one, cfa_channel_at(x, y, geom(pattern, -1, 0)));
                    assert_eq!(plus_one, cfa_channel_at(x, y, geom(pattern, -3, 0)));

                    let plus_one_y = cfa_channel_at(x, y, geom(pattern, 0, 1));
                    assert_eq!(plus_one_y, cfa_channel_at(x, y, geom(pattern, 0, -1)));
                    assert_eq!(plus_one_y, cfa_channel_at(x, y, geom(pattern, 0, -5)));

                    let zero = cfa_channel_at(x, y, geom(pattern, 0, 0));
                    assert_eq!(zero, cfa_channel_at(x, y, geom(pattern, -2, -2)));
                }
            }
        }
    }

    /// Paint a 6x6 RGGB mosaic with one constant per colour; the central-third
    /// window (x in 2..4, y in 2..4) must recover those constants exactly.
    fn mosaic_6x6_rggb() -> Vec<f32> {
        // Painted from the RGGB definition directly, NOT from the function
        // under test: even/even is R, odd/odd is B, the diagonal is G.
        let mut data = vec![0f32; 36];
        for y in 0..6usize {
            for x in 0..6usize {
                data[y * 6 + x] = match (x % 2 == 0, y % 2 == 0) {
                    (true, true) => 2000.0,
                    (false, false) => 1000.0,
                    _ => 4000.0,
                };
            }
        }
        data
    }

    #[test]
    fn channel_means_recover_painted_constants() {
        let data = mosaic_6x6_rggb();
        let means = central_third_channel_means(&data, 6, 6, geom(Bayer::Rggb, 0, 0));
        assert_eq!(means, [2000.0, 4000.0, 1000.0]);
    }

    /// The offsets are honoured by the means, not just by the mapping: reading
    /// the same painted mosaic one column out of phase relabels the pixels.
    /// Window pixels are (2,2)=R-paint, (3,2)=G, (2,3)=G, (3,3)=B; with xoff=1
    /// they are labelled G, R, B, G respectively.
    #[test]
    fn channel_means_follow_the_offset() {
        let data = mosaic_6x6_rggb();
        let means = central_third_channel_means(&data, 6, 6, geom(Bayer::Rggb, 1, 0));
        assert_eq!(means, [4000.0, 1500.0, 4000.0]);
    }

    /// Same window as the mono `central_third_mean`: recombining the channel
    /// means by their pixel counts reproduces it, over a ramp where any
    /// difference in window bounds moves the answer.
    ///
    /// Sizes are chosen so the assertion can see a mis-spelled window: at a
    /// multiple of 3 the two obvious spellings `(2 * w) / 3` and `w - w / 3`
    /// agree, so 9x9 alone proves nothing about which one is in force. 10x10
    /// (6 vs 7) and 7x5 (4 vs 5 columns, 3 vs 4 rows) separate them.
    #[test]
    fn window_matches_mono_central_third_mean() {
        for (w, h, expected_pixels) in [(9usize, 9usize, 9u64), (10, 10, 9), (7, 5, 4)] {
            let data: Vec<f32> = (0..w * h).map(|i| i as f32 * 1.5 + 3.0).collect();
            let g = geom(Bayer::Rggb, 0, 0);
            let means = central_third_channel_means(&data, w, h, g);

            let mut counts = [0u64; 3];
            for y in h / 3..(2 * h) / 3 {
                for x in w / 3..(2 * w) / 3 {
                    counts[cfa_channel_at(x, y, g).idx()] += 1;
                }
            }
            let total: u64 = counts.iter().sum();
            assert_eq!(total, expected_pixels, "window size for {w}x{h}");
            let recombined =
                (0..3).map(|c| means[c] * counts[c] as f64).sum::<f64>() / total as f64;
            let mono = super::super::engine::central_third_mean(&data, w, h);
            assert!(
                (recombined - mono).abs() < 1e-9,
                "{w}x{h}: recombined {recombined} vs mono {mono}"
            );
        }
    }

    /// A window too small to contain every colour reports 0.0 for the missing
    /// ones — never NaN out of a 0/0 division. Only reachable on degenerate
    /// images, but the guard is what keeps a downstream scale factor finite.
    #[test]
    fn empty_channels_are_zero_not_nan() {
        // 3x3 -> window is the single pixel (1,1), which is B under RGGB.
        let data = vec![7.0f32; 9];
        let means = central_third_channel_means(&data, 3, 3, geom(Bayer::Rggb, 0, 0));
        assert_eq!(means, [0.0, 0.0, 7.0]);
        assert!(means.iter().all(|m| m.is_finite()));
    }

    /// A window that collapses to nothing (a 1-pixel-tall image) samples no
    /// pixel at all. This is the documented divergence from the mono mean,
    /// which widens a collapsed window to one row; no real frame reaches it.
    #[test]
    fn collapsed_window_reports_zeros() {
        let data = vec![7.0f32; 4];
        let means = central_third_channel_means(&data, 4, 1, geom(Bayer::Rggb, 0, 0));
        assert_eq!(means, [0.0, 0.0, 0.0]);
    }
}

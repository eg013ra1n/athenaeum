//! Crop to coverage (spec §5.9, plan 6b-3 D10): the rectangle a run keeps,
//! and the pure operations that apply it to a plane, a header and a file.
//!
//! The rectangle is in array pixels of the master (the grid, for a
//! project). A drizzled output uses the same rectangle `scale` times finer
//! ([`CropRect::scaled`]): every master pixel is `s × s` drizzle pixels, and
//! the drizzle WCS (`scale_plate_solve`) is defined on exactly that lattice.
//! Cropping moves only `CRPIX`: SIP terms are polynomials in
//! `(u, v) = p − CRPIX`, so they hold unchanged, and the WCS refers to array
//! coordinates, so the row order needs no special case.
//!
//! This module holds the rectangle and the plane operations only; rewriting a file's header and cropping a file on
//! disk belong to the caller.

use serde::{Deserialize, Serialize};

/// Columns `x0 .. x0 + width`, rows `y0 .. y0 + height`, in array pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CropRect {
    /// First kept column, in array pixels (0-based).
    pub x0: usize,
    /// First kept row, in array pixels (0-based).
    pub y0: usize,
    /// Number of kept columns, in pixels.
    pub width: usize,
    /// Number of kept rows, in pixels.
    pub height: usize,
}

impl CropRect {
    /// The whole `width × height` image — what an uncropped finalization
    /// "crops" to.
    pub fn full(width: usize, height: usize) -> Self {
        Self {
            x0: 0,
            y0: 0,
            width,
            height,
        }
    }

    /// Whether this rectangle is exactly the whole `width × height` image (nothing is cropped).
    pub fn is_full(&self, width: usize, height: usize) -> bool {
        *self == Self::full(width, height)
    }

    /// Non-empty and inside a `width × height` image.
    pub fn fits_within(&self, width: usize, height: usize) -> bool {
        self.width > 0
            && self.height > 0
            && self.x0 + self.width <= width
            && self.y0 + self.height <= height
    }

    /// The smallest rectangle holding both.
    pub fn union(self, other: CropRect) -> CropRect {
        let x0 = self.x0.min(other.x0);
        let y0 = self.y0.min(other.y0);
        let x1 = (self.x0 + self.width).max(other.x0 + other.width);
        let y1 = (self.y0 + self.height).max(other.y0 + other.height);
        CropRect {
            x0,
            y0,
            width: x1 - x0,
            height: y1 - y0,
        }
    }

    /// The same rectangle on an output `s` times finer (drizzle).
    pub fn scaled(self, s: u32) -> CropRect {
        let s = s as usize;
        CropRect {
            x0: self.x0 * s,
            y0: self.y0 * s,
            width: self.width * s,
            height: self.height * s,
        }
    }
}

/// A pixel is covered when its count is finite and at least `k`: NaN and
/// either infinity never qualify (a count is a whole number stored as f32).
fn covers(count: f32, k: f32) -> bool {
    count.is_finite() && count >= k
}

/// A plane whose length disagrees with its dimensions is refused (logged,
/// never a panic or an out-of-range slice).
fn plane_matches(coverage: &[f32], width: usize, height: usize) -> bool {
    if coverage.len() == width * height {
        return true;
    }
    tracing::warn!(
        len = coverage.len(),
        width,
        height,
        "coverage plane length does not match the image"
    );
    false
}

/// The bounding box of the pixels at least `k` frames cover; `None` when no
/// pixel reaches `k`. A count is a whole number stored as f32, so `>=` is
/// exact; a NaN never qualifies, and neither does an infinity.
pub fn coverage_rect(coverage: &[f32], width: usize, height: usize, k: u32) -> Option<CropRect> {
    if !plane_matches(coverage, width, height) {
        return None;
    }
    let k = k as f32;
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
    for y in 0..height {
        let row = &coverage[y * width..(y + 1) * width];
        let first = row.iter().position(|&c| covers(c, k));
        let Some(first) = first else { continue };
        let last = row.iter().rposition(|&c| covers(c, k)).unwrap_or(first);
        x0 = x0.min(first);
        x1 = x1.max(last);
        y0 = y0.min(y);
        y1 = y1.max(y);
    }
    (x0 != usize::MAX).then(|| CropRect {
        x0,
        y0,
        width: x1 - x0 + 1,
        height: y1 - y0 + 1,
    })
}

/// The largest-area rectangle whose EVERY pixel at least `k` frames cover
/// (plan 6b-3 review I1: a grid master's statistics are measured on covered
/// pixels only, and `measure_plane` takes a rectangle). The histogram
/// method, one pass per row, `O(width · height)`; of equal areas the first
/// found in row order wins. `None` when no pixel reaches `k` (NaN never
/// does).
pub fn largest_covered_rect(
    coverage: &[f32],
    width: usize,
    height: usize,
    k: u32,
) -> Option<CropRect> {
    if !plane_matches(coverage, width, height) {
        return None;
    }
    let k = k as f32;
    let mut heights = vec![0usize; width];
    let mut stack: Vec<usize> = Vec::with_capacity(width + 1);
    let mut best: Option<(usize, CropRect)> = None;
    for y in 0..height {
        let row = &coverage[y * width..(y + 1) * width];
        for (x, h) in heights.iter_mut().enumerate() {
            *h = if covers(row[x], k) { *h + 1 } else { 0 };
        }
        stack.clear();
        for x in 0..=width {
            let current = if x < width { heights[x] } else { 0 };
            while let Some(&top) = stack.last() {
                if heights[top] <= current {
                    break;
                }
                stack.pop();
                let h = heights[top];
                let left = stack.last().map_or(0, |&l| l + 1);
                let area = h * (x - left);
                if best.is_none_or(|(a, _)| area > a) {
                    best = Some((
                        area,
                        CropRect {
                            x0: left,
                            y0: y + 1 - h,
                            width: x - left,
                            height: h,
                        },
                    ));
                }
            }
            if x < width {
                stack.push(x);
            }
        }
    }
    best.map(|(_, r)| r)
}

/// The union of every rectangle; `None` for none.
pub fn union_rects(rects: impl IntoIterator<Item = CropRect>) -> Option<CropRect> {
    rects.into_iter().reduce(CropRect::union)
}

/// One plane's `rect`, row by row. `rect` must fit the plane.
pub fn crop_plane(plane: &[f32], width: usize, rect: CropRect) -> Vec<f32> {
    let mut out = Vec::with_capacity(rect.width * rect.height);
    for y in rect.y0..rect.y0 + rect.height {
        let start = y * width + rect.x0;
        out.extend_from_slice(&plane[start..start + rect.width]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mutation survivor (crop.rs `fits_within`): a rectangle ending exactly
    /// on the plane edge fits; one more pixel on any side does not.
    #[test]
    fn fits_within_is_exact_at_the_plane_edge() {
        let r = CropRect {
            x0: 3,
            y0: 2,
            width: 5,
            height: 4,
        };
        assert!(r.fits_within(8, 6), "ends exactly at the edge");
        assert!(!r.fits_within(7, 6), "one column too many");
        assert!(!r.fits_within(8, 5), "one row too many");
        assert!(r.fits_within(9, 7));
        assert!(!CropRect { width: 0, ..r }.fits_within(8, 6));
        assert!(!CropRect { height: 0, ..r }.fits_within(8, 6));
    }

    /// Mutation survivors (crop.rs `union`): the far edge is `x0 + width`
    /// and the union's size is `x1 - x0`, checked on inputs where sum,
    /// product and difference all differ.
    #[test]
    fn union_is_the_exact_bounding_rectangle() {
        let a = CropRect {
            x0: 3,
            y0: 4,
            width: 5,
            height: 6,
        };
        let b = CropRect {
            x0: 7,
            y0: 2,
            width: 9,
            height: 3,
        };
        // x: 3..8 and 7..16 -> 3..16; y: 4..10 and 2..5 -> 2..10.
        let expect = CropRect {
            x0: 3,
            y0: 2,
            width: 13,
            height: 8,
        };
        assert_eq!(a.union(b), expect);
        assert_eq!(b.union(a), expect);
        // A rectangle that holds the other keeps its own size.
        let inner = CropRect {
            x0: 4,
            y0: 5,
            width: 2,
            height: 3,
        };
        assert_eq!(a.union(inner), a);
        assert_eq!(a.union(a), a);
    }

    /// Mutation survivor (crop.rs `scaled`): every field multiplies.
    #[test]
    fn scaled_multiplies_every_field() {
        let r = CropRect {
            x0: 5,
            y0: 7,
            width: 11,
            height: 13,
        };
        assert_eq!(
            r.scaled(3),
            CropRect {
                x0: 15,
                y0: 21,
                width: 33,
                height: 39,
            }
        );
        assert_eq!(r.scaled(1), r);
    }

    /// Plan 6b-3 D10: the rectangle bounds exactly the pixels at or above
    /// `k`, ignores NaN, and is `None` when nothing reaches `k`.
    #[test]
    fn coverage_rect_bounds_the_pixels_at_or_above_k() {
        let (w, h) = (8, 6);
        let mut cov = vec![0.0f32; w * h];
        for y in 1..5 {
            for x in 2..7 {
                cov[y * w + x] = 2.0;
            }
        }
        cov[2 * w + 3] = 3.0;
        cov[4 * w + 6] = 3.0;
        cov[5 * w + 7] = f32::NAN;
        assert_eq!(
            coverage_rect(&cov, w, h, 1),
            Some(CropRect {
                x0: 2,
                y0: 1,
                width: 5,
                height: 4
            })
        );
        assert_eq!(
            coverage_rect(&cov, w, h, 2),
            Some(CropRect {
                x0: 2,
                y0: 1,
                width: 5,
                height: 4
            })
        );
        assert_eq!(
            coverage_rect(&cov, w, h, 3),
            Some(CropRect {
                x0: 3,
                y0: 2,
                width: 4,
                height: 3
            })
        );
        assert_eq!(coverage_rect(&cov, w, h, 4), None);
    }

    /// Plan 6b-3 D10: the run-wide rectangle is the union; a drizzle
    /// rectangle is the master's `s` times finer; fit checks both edges.
    #[test]
    fn rectangles_unite_scale_and_fit() {
        let a = CropRect {
            x0: 2,
            y0: 1,
            width: 5,
            height: 4,
        };
        let b = CropRect {
            x0: 4,
            y0: 0,
            width: 6,
            height: 2,
        };
        assert_eq!(
            union_rects([a, b]),
            Some(CropRect {
                x0: 2,
                y0: 0,
                width: 8,
                height: 5
            })
        );
        assert_eq!(union_rects(std::iter::empty()), None);
        assert_eq!(
            a.scaled(2),
            CropRect {
                x0: 4,
                y0: 2,
                width: 10,
                height: 8
            }
        );
        assert!(a.fits_within(7, 5) && !a.fits_within(6, 5) && !a.fits_within(7, 4));
        assert!(!CropRect {
            x0: 0,
            y0: 0,
            width: 0,
            height: 3
        }
        .fits_within(4, 4));
        assert!(CropRect::full(7, 5).is_full(7, 5) && !a.is_full(7, 5));
    }

    /// Review I1: the largest fully covered rectangle holds covered pixels
    /// only — of an L-shaped coverage it keeps the bigger arm, never a corner
    /// nothing covers — and there is none without a covered pixel.
    #[test]
    fn the_largest_covered_rectangle_holds_covered_pixels_only() {
        let (w, h) = (10usize, 8usize);
        let mut cov = vec![0.0f32; w * h];
        for y in 0..h {
            for x in 0..6 {
                cov[y * w + x] = 2.0;
            }
        }
        for y in 5..8 {
            for x in 6..10 {
                cov[y * w + x] = 1.0;
            }
        }
        let r = largest_covered_rect(&cov, w, h, 1).unwrap();
        assert_eq!(
            r,
            CropRect {
                x0: 0,
                y0: 0,
                width: 6,
                height: 8
            },
            "48 px beats the 10 × 3 strip"
        );
        for y in r.y0..r.y0 + r.height {
            for x in r.x0..r.x0 + r.width {
                assert!(cov[y * w + x] >= 1.0, "({x}, {y}) is uncovered");
            }
        }
        cov[3 * w + 2] = 0.0;
        let holed = largest_covered_rect(&cov, w, h, 1).unwrap();
        assert!(
            holed.width * holed.height < 48 && holed.width * holed.height >= 24,
            "{holed:?}"
        );
        assert_eq!(largest_covered_rect(&vec![0.0; w * h], w, h, 1), None);
        assert_eq!(largest_covered_rect(&vec![f32::NAN; w * h], w, h, 1), None);
    }

    /// Edge cases of both rectangles: nothing covered, everything covered,
    /// one pixel, a hole, non-finite counts, `k` above every count, a
    /// 1-pixel-wide image, an empty plane and a mismatched length.
    #[test]
    fn coverage_rectangles_handle_the_edge_cases() {
        let both = |cov: &[f32], w: usize, h: usize, k: u32| {
            (
                coverage_rect(cov, w, h, k),
                largest_covered_rect(cov, w, h, k),
            )
        };
        let (w, h) = (7usize, 5usize);
        assert_eq!(both(&vec![0.0; w * h], w, h, 1), (None, None));
        let full = Some(CropRect::full(w, h));
        assert_eq!(both(&vec![3.0; w * h], w, h, 3), (full, full));
        assert_eq!(both(&vec![3.0; w * h], w, h, 0), (full, full));
        let mut one = vec![0.0f32; w * h];
        one[3 * w + 4] = 1.0;
        let px = Some(CropRect {
            x0: 4,
            y0: 3,
            width: 1,
            height: 1,
        });
        assert_eq!(both(&one, w, h, 1), (px, px));
        // A hole in the middle: the bounding box is still full, the largest
        // covered rectangle avoids the hole (5 columns x 5 rows, or 7 x 2).
        let mut holed = vec![1.0f32; w * h];
        holed[2 * w + 3] = 0.0;
        assert_eq!(coverage_rect(&holed, w, h, 1), full);
        let r = largest_covered_rect(&holed, w, h, 1).unwrap();
        assert_eq!(r.width * r.height, 15, "{r:?}");
        for y in r.y0..r.y0 + r.height {
            for x in r.x0..r.x0 + r.width {
                assert!(holed[y * w + x] >= 1.0, "({x}, {y}) is the hole");
            }
        }
        // Non-finite counts never qualify, +inf included.
        let mut odd = vec![f32::NAN; w * h];
        odd[w + 1] = f32::INFINITY;
        odd[w + 2] = f32::NEG_INFINITY;
        odd[w + 3] = 2.0;
        let cell = Some(CropRect {
            x0: 3,
            y0: 1,
            width: 1,
            height: 1,
        });
        assert_eq!(both(&odd, w, h, 1), (cell, cell));
        // K above the maximum count.
        assert_eq!(both(&vec![2.0; w * h], w, h, 3), (None, None));
        assert_eq!(both(&vec![2.0; w * h], w, h, u32::MAX), (None, None));
        // A 1-pixel-wide image.
        let col = [0.0, 1.0, 1.0, 0.0, 1.0, 1.0, 1.0];
        assert_eq!(
            coverage_rect(&col, 1, 7, 1),
            Some(CropRect {
                x0: 0,
                y0: 1,
                width: 1,
                height: 6
            })
        );
        assert_eq!(
            largest_covered_rect(&col, 1, 7, 1),
            Some(CropRect {
                x0: 0,
                y0: 4,
                width: 1,
                height: 3
            })
        );
        // A 1-pixel-high image.
        assert_eq!(
            largest_covered_rect(&col, 7, 1, 1),
            Some(CropRect {
                x0: 4,
                y0: 0,
                width: 3,
                height: 1
            })
        );
        // Empty and mismatched planes are refused, never a panic.
        assert_eq!(both(&[], 0, 0, 1), (None, None));
        assert_eq!(both(&[], 0, 4, 1), (None, None));
        assert_eq!(both(&[1.0; 5], 3, 2, 1), (None, None));
    }

    #[test]
    fn crop_plane_takes_the_rectangle_row_by_row() {
        let (w, h) = (5, 4);
        let plane: Vec<f32> = (0..w * h).map(|i| i as f32).collect();
        let out = crop_plane(
            &plane,
            w,
            CropRect {
                x0: 1,
                y0: 2,
                width: 3,
                height: 2,
            },
        );
        assert_eq!(out, vec![11.0, 12.0, 13.0, 16.0, 17.0, 18.0]);
    }
}

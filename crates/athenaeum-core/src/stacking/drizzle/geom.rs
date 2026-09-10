//! Drizzle geometry (M3 Task 1, spec §7, rulings R-M3-1..R-M3-3): the pure
//! math the drizzle stage driver (a later task) calls per source pixel —
//! the reference ↔ output-grid coordinate map, the four corners of a
//! source pixel's shrunk "drop", forward-mapping those corners subject →
//! reference → output grid through a frame's [`PixelMap`], exact
//! convex-quad ∩ unit-pixel clipping (Sutherland–Hodgman + shoelace), and
//! the 16×16 tabulated micro-drop kernel table for `circle`/`gaussian`
//! (`square` uses [`clip_area`] directly and has no table).
//!
//! Coordinate convention (the whole pipeline's, spec-wide): integer pixel
//! coordinates are pixel CENTRES — pixel `(x, y)` covers
//! `[x − 0.5, x + 0.5] × [y − 0.5, y + 0.5]` — on both the subject/reference
//! side and the output-grid side.

use crate::geometry::pixel_map::PixelMap;
use crate::stacking::config::DrizzleKernel;

/// A convex quadrilateral in output-grid coordinates, vertex order matching
/// [`drop_corners`] (nominally counter-clockwise; [`map_drop`] restores that
/// orientation when a flipped [`PixelMap`] mirrors it).
pub type Quad = [(f64, f64); 4];

/// The 16×16 tabulated-kernel size (ruling R-M3-3): 256 sub-drops per
/// source pixel for the `circle`/`gaussian` kernels.
pub const KERNEL_GRID_SIZE: usize = 16;

/// The Gaussian kernel's edge weight (ruling R-M3-3): `σ` is chosen so that
/// `exp(-(dropShrink/2)² / (2σ²)) == KERNEL_EPSILON` — the drop's nominal
/// radius carries `2.5%` of the centre's density.
pub const KERNEL_EPSILON: f64 = 0.025;

/// Output-grid coordinate of a reference coordinate: `v = s·u + (s − 1)/2`
/// (pixel centres convention on both sides; reference pixel `i` covers
/// output pixels `s·i .. s·i + s − 1`).
#[inline]
pub fn to_output(u: f64, scale: u32) -> f64 {
    debug_assert!(scale >= 1, "drizzle scale must be >= 1 (R-M3-10), got 0");
    let s = scale as f64;
    s * u + (s - 1.0) / 2.0
}

/// Reference coordinate of an output coordinate — the inverse of
/// [`to_output`].
#[inline]
pub fn to_reference(v: f64, scale: u32) -> f64 {
    debug_assert!(scale >= 1, "drizzle scale must be >= 1 (R-M3-10), got 0");
    let s = scale as f64;
    (v - (s - 1.0) / 2.0) / s
}

/// The four corners of source pixel `(x, y)`'s drop, counter-clockwise:
/// `(x − h, y − h), (x + h, y − h), (x + h, y + h), (x − h, y + h)` with
/// `h = drop_shrink / 2`.
pub fn drop_corners(x: usize, y: usize, drop_shrink: f64) -> [(f64, f64); 4] {
    let h = drop_shrink / 2.0;
    let (xf, yf) = (x as f64, y as f64);
    [
        (xf - h, yf - h),
        (xf + h, yf - h),
        (xf + h, yf + h),
        (xf - h, yf + h),
    ]
}

/// Map the four corners subject → reference → output grid. Returns the quad
/// and its integer bounding box `(x_min, y_min, x_max, y_max)` in output
/// pixels — inclusive, the pixels whose `[i − 0.5, i + 0.5]` extent overlaps
/// the quad's coordinate span with POSITIVE length (a quad edge landing
/// exactly on a pixel boundary contributes zero area on either side and is
/// excluded from both) — or `None` when any mapped coordinate is not
/// finite.
///
/// The single-point version of "the pixel whose extent contains a
/// coordinate `v`" is `floor(v + 0.5)`, deliberately **not** `round(v)`:
/// Rust's `f64::round` rounds half-away-from-zero, which disagrees with
/// `floor(v + 0.5)` at every negative half-integer (`round(-0.5) == -1` but
/// `floor(-0.5 + 0.5) == 0`), and this stage's mapped coordinates go
/// negative routinely (an off-frame registration, or a mirrored frame like
/// the `flipped_and_rotated_map_…` test below). The bbox itself goes one
/// step further than either formula: it uses the stricter positive-overlap
/// rule above for the whole continuous span (not just its two endpoints),
/// which is what a boundary-exact span needs and neither `round` nor a
/// bare `floor(v + 0.5)` resolves on its own — see `strict_ceil`/
/// `strict_floor`.
///
/// The returned bbox is **not** clamped to any output-grid extent and may
/// be empty (`x_max < x_min` / `y_max < y_min`, e.g. for `drop_shrink = 0`
/// or a locally singular mapping whose span collapses onto an exact
/// half-integer boundary) — callers must intersect it with their own
/// bounds before iterating; the stage driver (a later task) intersects
/// with its band rectangle.
///
/// Signed-area orientation fix: [`clip_area`] assumes a counter-clockwise
/// polygon; a flipped `PixelMap` mirrors the quad, so this reverses the
/// vertex order when the shoelace area of the mapped quad is negative.
pub fn map_drop(
    map: &PixelMap,
    corners: &[(f64, f64); 4],
    scale: u32,
) -> Option<(Quad, (i64, i64, i64, i64))> {
    let mut quad: Quad = [(0.0, 0.0); 4];
    for (i, &(sx, sy)) in corners.iter().enumerate() {
        let (u, v) = map.forward(sx, sy);
        let ox = to_output(u, scale);
        let oy = to_output(v, scale);
        if !ox.is_finite() || !oy.is_finite() {
            return None;
        }
        quad[i] = (ox, oy);
    }
    if signed_area(&quad) < 0.0 {
        quad.reverse();
    }

    let mut x_lo = f64::INFINITY;
    let mut x_hi = f64::NEG_INFINITY;
    let mut y_lo = f64::INFINITY;
    let mut y_hi = f64::NEG_INFINITY;
    for &(x, y) in &quad {
        x_lo = x_lo.min(x);
        x_hi = x_hi.max(x);
        y_lo = y_lo.min(y);
        y_hi = y_hi.max(y);
    }
    let bbox = (
        strict_ceil(x_lo - 0.5),
        strict_ceil(y_lo - 0.5),
        strict_floor(x_hi + 0.5),
        strict_floor(y_hi + 0.5),
    );
    Some((quad, bbox))
}

/// Signed polygon area by the shoelace formula — positive for the vertex
/// order [`drop_corners`] produces (see the identity-map pin in the tests
/// below), which this module treats as "counter-clockwise" throughout.
fn signed_area(pts: &[(f64, f64)]) -> f64 {
    let n = pts.len();
    if n < 3 {
        return 0.0;
    }
    // Carry `prev` across the loop instead of an `(i + 1) % n` index per
    // vertex: mathematically the same cyclic sum (shoelace is invariant
    // under a cyclic shift of the starting vertex), just without the
    // per-vertex modulo.
    let mut sum = 0.0;
    let mut prev = pts[n - 1];
    for &curr in pts {
        sum += prev.0 * curr.1 - curr.0 * prev.1;
        prev = curr;
    }
    0.5 * sum
}

/// The smallest integer STRICTLY greater than `v` — like `ceil`, except a
/// `v` that is already an exact integer rounds up to the next one instead
/// of staying put. Used so a quad edge landing exactly on a pixel boundary
/// excludes the pixel on the low side (see [`map_drop`]'s doc).
#[inline]
fn strict_ceil(v: f64) -> i64 {
    let f = v.floor();
    if v == f {
        f as i64 + 1
    } else {
        v.ceil() as i64
    }
}

/// The largest integer STRICTLY less than `v` — the [`strict_ceil`] mirror,
/// excluding the pixel on the high side of an exact boundary.
#[inline]
fn strict_floor(v: f64) -> i64 {
    let c = v.ceil();
    if v == c {
        c as i64 - 1
    } else {
        v.floor() as i64
    }
}

/// Area of `quad ∩ [px − 0.5, px + 0.5] × [py − 0.5, py + 0.5]` by
/// Sutherland–Hodgman clipping (four half-planes) and the shoelace formula;
/// `quad` must be convex (an affine or mildly distorted image of a square
/// is), any orientation. Returns 0 when the polygon is empty.
///
/// Pure `f64`, no allocation: clipping a convex quad against one half-plane
/// grows its vertex count by at most one, so four sequential half-plane
/// clips of a 4-vertex quad never exceed 8 vertices — two fixed 16-slot
/// buffers are ample headroom.
///
/// Ping-pongs between the two buffers by swapping the `&mut` REFERENCES
/// (`cur`/`nxt`, each a pointer-sized `&mut [(f64, f64); 16]`), never the
/// 256-byte arrays themselves — swapping the arrays by value defeated LLVM's
/// SROA (the dynamic `count`/`n` indexing keeps them off the stack in
/// registers either way) and cost a measured 2.4× per call in this stage's
/// innermost loop (one call per source-pixel × overlapped-output-pixel
/// pair).
pub fn clip_area(quad: &Quad, px: i64, py: i64) -> f64 {
    let x_lo = px as f64 - 0.5;
    let x_hi = px as f64 + 0.5;
    let y_lo = py as f64 - 0.5;
    let y_hi = py as f64 + 0.5;

    let mut buf_a: [(f64, f64); 16] = [(0.0, 0.0); 16];
    let mut buf_b: [(f64, f64); 16] = [(0.0, 0.0); 16];
    buf_a[..4].copy_from_slice(&quad[..]);
    let mut cur = &mut buf_a;
    let mut nxt = &mut buf_b;
    let mut n = 4usize;

    macro_rules! clip_pass {
        ($inside:expr, $intersect:expr) => {{
            n = clip_half_plane(&cur[..n], nxt, $inside, $intersect);
            std::mem::swap(&mut cur, &mut nxt);
            if n == 0 {
                return 0.0;
            }
        }};
    }

    clip_pass!(
        |(x, _): (f64, f64)| x >= x_lo,
        |p0: (f64, f64), p1: (f64, f64)| {
            let t = (x_lo - p0.0) / (p1.0 - p0.0);
            (x_lo, p0.1 + t * (p1.1 - p0.1))
        }
    );
    clip_pass!(
        |(x, _): (f64, f64)| x <= x_hi,
        |p0: (f64, f64), p1: (f64, f64)| {
            let t = (x_hi - p0.0) / (p1.0 - p0.0);
            (x_hi, p0.1 + t * (p1.1 - p0.1))
        }
    );
    clip_pass!(
        |(_, y): (f64, f64)| y >= y_lo,
        |p0: (f64, f64), p1: (f64, f64)| {
            let t = (y_lo - p0.1) / (p1.1 - p0.1);
            (p0.0 + t * (p1.0 - p0.0), y_lo)
        }
    );
    clip_pass!(
        |(_, y): (f64, f64)| y <= y_hi,
        |p0: (f64, f64), p1: (f64, f64)| {
            let t = (y_hi - p0.1) / (p1.1 - p0.1);
            (p0.0 + t * (p1.0 - p0.0), y_hi)
        }
    );

    signed_area(&cur[..n]).abs()
}

/// One Sutherland–Hodgman clip pass against a half-plane (`inside`), adding
/// the edge/boundary intersection (`intersect`) whenever consecutive
/// vertices cross it. Writes the clipped polygon into `output` and returns
/// its vertex count.
fn clip_half_plane(
    input: &[(f64, f64)],
    output: &mut [(f64, f64); 16],
    inside: impl Fn((f64, f64)) -> bool,
    intersect: impl Fn((f64, f64), (f64, f64)) -> (f64, f64),
) -> usize {
    let n = input.len();
    if n == 0 {
        return 0;
    }
    // Carry the previous vertex's `inside` flag across the loop instead of
    // recomputing it for both `prev` and `curr` every iteration — `n + 1`
    // evaluations instead of `2n`.
    let mut count = 0;
    let mut prev = input[n - 1];
    let mut prev_in = inside(prev);
    for &curr in input {
        let curr_in = inside(curr);
        if curr_in {
            if !prev_in {
                output[count] = intersect(prev, curr);
                count += 1;
            }
            output[count] = curr;
            count += 1;
        } else if prev_in {
            output[count] = intersect(prev, curr);
            count += 1;
        }
        prev = curr;
        prev_in = curr_in;
    }
    count
}

/// The 16 × 16 micro-drop table for the tabulated kernels (ruling R-M3-3):
/// sub-drop centre offsets from the pixel centre (source-pixel units) and
/// weights summing to `drop_shrink²`.
#[derive(Debug, Clone)]
pub struct KernelTable {
    pub offsets: Vec<(f64, f64)>,
    pub weights: Vec<f64>,
}

/// Builds the 256-entry micro-drop table for `circle`/`gaussian` (ruling
/// R-M3-3); `None` for `square`, which uses [`clip_area`] directly and has
/// no table. The drop (side `drop_shrink`) is subdivided into a
/// `KERNEL_GRID_SIZE × KERNEL_GRID_SIZE` grid of sub-drops of side
/// `drop_shrink / 16`; each carries the kernel's weight at its own centre
/// (circle: 1 inside radius `drop_shrink / 2`, 0 outside; gaussian:
/// `exp(−r² / (2σ²))`, `σ = (drop_shrink / 2) / sqrt(−2·ln(KERNEL_EPSILON))`),
/// then every weight is rescaled so the 256 of them sum to `drop_shrink²`
/// **in SOURCE-pixel units** — the drop's own area before any mapping.
/// [`clip_area`] measures the mapped drop in OUTPUT-pixel units instead,
/// i.e. `s²` times as much (ruling R-M3-2) — the two are not directly
/// comparable, and the stage driver's `I`/`W` accumulator (`I += a·w·N`,
/// `W += a·w`, final value `I / W`) cancels any constant factor common to
/// every deposit, so this difference costs nothing in practice. It is
/// flagged here so a later reader does not "fix" a perceived mismatch by
/// inserting a stray `s²` into this function.
pub fn kernel_table(kernel: DrizzleKernel, drop_shrink: f64) -> Option<KernelTable> {
    if kernel == DrizzleKernel::Square {
        return None;
    }
    debug_assert!(
        drop_shrink.is_finite() && drop_shrink > 0.0,
        "drop_shrink must be finite and positive, got {drop_shrink}"
    );
    let h = drop_shrink / 2.0;
    let cell = drop_shrink / KERNEL_GRID_SIZE as f64;
    let sigma = h / (-2.0 * KERNEL_EPSILON.ln()).sqrt();

    let n = KERNEL_GRID_SIZE * KERNEL_GRID_SIZE;
    let mut offsets = Vec::with_capacity(n);
    let mut raw = Vec::with_capacity(n);
    for j in 0..KERNEL_GRID_SIZE {
        let oy = -h + (j as f64 + 0.5) * cell;
        for i in 0..KERNEL_GRID_SIZE {
            let ox = -h + (i as f64 + 0.5) * cell;
            let r2 = ox * ox + oy * oy;
            let w = match kernel {
                DrizzleKernel::Circle => {
                    if r2 <= h * h {
                        1.0
                    } else {
                        0.0
                    }
                }
                DrizzleKernel::Gaussian => (-r2 / (2.0 * sigma * sigma)).exp(),
                DrizzleKernel::Square => unreachable!("returned above"),
            };
            offsets.push((ox, oy));
            raw.push(w);
        }
    }
    let sum: f64 = raw.iter().sum();
    let target = drop_shrink * drop_shrink;
    let scale = if sum > 0.0 { target / sum } else { 0.0 };
    let weights: Vec<f64> = raw.into_iter().map(|w| w * scale).collect();
    Some(KernelTable { offsets, weights })
}

#[cfg(test)]
mod tests {
    use crate::geometry::linear::{Linear, LinearKind};
    use crate::geometry::pixel_map::PixelMap;
    use crate::stacking::config::DrizzleKernel;
    use crate::stacking::drizzle::geom::*;

    fn identity_map() -> PixelMap {
        PixelMap::linear(Linear::identity()).unwrap()
    }

    fn translation_map(dx: f64, dy: f64) -> PixelMap {
        PixelMap::linear(Linear {
            kind: LinearKind::Affine,
            m: [[1.0, 0.0, dx], [0.0, 1.0, dy], [0.0, 0.0, 1.0]],
        })
        .unwrap()
    }

    fn rotation_map(deg: f64) -> PixelMap {
        let r = deg.to_radians();
        let (s, c) = r.sin_cos();
        PixelMap::linear(Linear {
            kind: LinearKind::Affine,
            m: [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]],
        })
        .unwrap()
    }

    // (a)
    #[test]
    fn to_output_round_trips_and_pins_reference_points() {
        for s in [1u32, 2, 3] {
            for u in [-3.25_f64, -1.0, 0.0, 0.5, 2.0, 17.75] {
                let v = to_output(u, s);
                let back = to_reference(v, s);
                assert!((back - u).abs() < 1e-12, "s={s} u={u} back={back}");
            }
        }
        assert_eq!(to_output(0.0, 2), 0.5);
        assert_eq!(to_output(-0.5, 2), -0.5);
        assert_eq!(to_output(0.5, 3), 2.5);
    }

    // (b)
    #[test]
    fn identity_scale1_drop_lands_exactly_on_its_own_pixel() {
        let map = identity_map();
        let corners = drop_corners(3, 4, 1.0);
        let (quad, _bbox) = map_drop(&map, &corners, 1).expect("finite map");
        let area_here = clip_area(&quad, 3, 4);
        assert!((area_here - 1.0).abs() < 1e-12, "area_here={area_here}");
        let area_neighbor = clip_area(&quad, 4, 4);
        assert_eq!(area_neighbor, 0.0);
    }

    // (c)
    #[test]
    fn identity_scale2_drop_splits_evenly_across_four_output_pixels() {
        let map = identity_map();
        for (drop_shrink, expected) in [(1.0, 1.0), (0.5, 0.25)] {
            let corners = drop_corners(0, 0, drop_shrink);
            let (quad, bbox) = map_drop(&map, &corners, 2).expect("finite map");
            assert_eq!(bbox, (0, 0, 1, 1), "drop_shrink={drop_shrink}");
            let mut total = 0.0;
            for (px, py) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let a = clip_area(&quad, px, py);
                assert!(
                    (a - expected).abs() < 1e-12,
                    "drop_shrink={drop_shrink} px={px} py={py} a={a}"
                );
                total += a;
            }
            let scale_sq_drop_sq = 4.0 * drop_shrink * drop_shrink;
            assert!((total - scale_sq_drop_sq).abs() < 1e-9);
        }
    }

    // (d)
    #[test]
    fn translation_drop_bbox_excludes_boundary_touching_pixels() {
        let map = translation_map(0.5, 0.0);
        let corners = drop_corners(0, 0, 1.0);
        let (quad, bbox) = map_drop(&map, &corners, 2).expect("finite map");
        assert_eq!(bbox, (1, 0, 2, 1));
        for row in [0i64, 1] {
            let a1 = clip_area(&quad, 1, row);
            let a2 = clip_area(&quad, 2, row);
            assert!((a1 - 1.0).abs() < 1e-12, "row={row} a1={a1}");
            assert!((a2 - 1.0).abs() < 1e-12, "row={row} a2={a2}");
            assert_eq!(clip_area(&quad, 0, row), 0.0, "row={row}");
            assert_eq!(clip_area(&quad, 3, row), 0.0, "row={row}");
        }
    }

    // (e)
    #[test]
    fn rotation_conserves_area_over_the_bbox() {
        let map = rotation_map(30.0);
        let corners = drop_corners(5, 5, 0.9);
        let (quad, (x_min, y_min, x_max, y_max)) = map_drop(&map, &corners, 1).expect("finite map");
        let mut total = 0.0;
        for py in y_min..=y_max {
            for px in x_min..=x_max {
                total += clip_area(&quad, px, py);
            }
        }
        assert!((total - 0.81).abs() < 1e-9, "total={total}");
    }

    // (f)
    #[test]
    fn flipped_and_rotated_map_reorders_to_ccw_and_conserves_area() {
        // A pure axis-aligned mirror maps a square drop to another
        // axis-aligned square, which can land entirely inside a single
        // output pixel — a degenerate "conservation" check that would pass
        // even if the clipping were badly broken, since no half-plane
        // actually cuts anything. Composing the mirror with a 30° rotation
        // (and a translation, for realism — the pipeline's own flipped
        // maps always carry one) keeps the negative determinant but makes
        // the mapped quad non-axis-aligned, so it genuinely straddles
        // multiple output pixels and the clip + orientation fix are both
        // really exercised.
        let r = 30f64.to_radians();
        let (s, c) = r.sin_cos();
        let map = PixelMap::linear(Linear {
            kind: LinearKind::Affine,
            m: [[-c, -s, 0.3], [-s, c, -0.2], [0.0, 0.0, 1.0]],
        })
        .unwrap();
        assert!(
            map.is_flipped(),
            "the composed map must keep a negative determinant"
        );

        let corners = drop_corners(5, 5, 0.9);
        let (quad, (x_min, y_min, x_max, y_max)) = map_drop(&map, &corners, 1).expect("finite map");
        assert!(
            signed_area(&quad) > 0.0,
            "expected a CCW quad after the flip fix"
        );
        assert!(
            (x_max - x_min + 1) * (y_max - y_min + 1) > 1,
            "expected the quad to straddle more than one output pixel: bbox=({x_min},{y_min},{x_max},{y_max})"
        );

        let mut total = 0.0;
        for py in y_min..=y_max {
            for px in x_min..=x_max {
                total += clip_area(&quad, px, py);
            }
        }
        assert!((total - 0.81).abs() < 1e-9, "total={total}");
    }

    // (g)
    #[test]
    fn circle_kernel_table_is_a_hard_radius_cutoff() {
        let drop_shrink = 0.9;
        let table = kernel_table(DrizzleKernel::Circle, drop_shrink).expect("circle has a table");
        assert_eq!(table.offsets.len(), 256);
        assert_eq!(table.weights.len(), 256);
        let sum: f64 = table.weights.iter().sum();
        assert!((sum - drop_shrink * drop_shrink).abs() < 1e-9, "sum={sum}");
        let h = drop_shrink / 2.0;
        for (&(ox, oy), &w) in table.offsets.iter().zip(table.weights.iter()) {
            assert!(w >= 0.0, "offset {:?} has a negative weight {w}", (ox, oy));
            let r = (ox * ox + oy * oy).sqrt();
            if r > h {
                assert_eq!(
                    w,
                    0.0,
                    "offset {:?} at r={r} > h={h} has nonzero weight",
                    (ox, oy)
                );
            }
        }
    }

    #[test]
    fn gaussian_kernel_table_peaks_at_centre_and_decays_outward() {
        let drop_shrink = 0.9;
        let table =
            kernel_table(DrizzleKernel::Gaussian, drop_shrink).expect("gaussian has a table");
        assert_eq!(table.offsets.len(), 256);
        let sum: f64 = table.weights.iter().sum();
        assert!((sum - drop_shrink * drop_shrink).abs() < 1e-9, "sum={sum}");

        let r2 = |&(ox, oy): &(f64, f64)| ox * ox + oy * oy;
        let center_idx = (0..table.offsets.len())
            .min_by(|&i, &j| {
                r2(&table.offsets[i])
                    .partial_cmp(&r2(&table.offsets[j]))
                    .unwrap()
            })
            .unwrap();
        let corner_idx = (0..table.offsets.len())
            .max_by(|&i, &j| {
                r2(&table.offsets[i])
                    .partial_cmp(&r2(&table.offsets[j]))
                    .unwrap()
            })
            .unwrap();
        let center_w = table.weights[center_idx];
        let corner_w = table.weights[corner_idx];
        let max_w = table.weights.iter().cloned().fold(f64::MIN, f64::max);

        assert!(
            (center_w - max_w).abs() < 1e-12,
            "the centre-most sub-drop should carry the table's largest weight: \
             center_w={center_w} max_w={max_w}"
        );
        // The literal 16x16-grid corner sits at r = h*sqrt(2) ~= 1.33h, well
        // past the dropShrink/2 = h radius KERNEL_EPSILON is calibrated at
        // (r = h), so its weight is well under epsilon, not "~= epsilon" (an
        // inscribed-circle-vs-square-corner fact, not an implementation
        // choice) -- see task-1-report.md for the derivation. Pin the
        // qualitative shape first (center is the max, corner is a small,
        // still nonzero, fraction of it)...
        assert!(
            corner_w < center_w * 0.05,
            "corner_w={corner_w} center_w={center_w}"
        );
        assert!(corner_w > 0.0, "the gaussian never hits an exact zero");

        // ...then an exact, analytically derivable pin on top: the corner
        // cell centres at i,j in {0,15} (r^2/h^2 = 2*(15/16)^2 = 1.7578125)
        // and the centre-most cells at i,j in {7,8} (r^2/h^2 =
        // 2*(0.5/16)^2 = 0.0078125), so corner_w/center_w =
        // exp(-(r_corner^2 - r_centre^2)/(2*sigma^2)) =
        // KERNEL_EPSILON^(delta(r^2)/h^2), and delta(r^2)/h^2 = 1.7578125
        // - 0.0078125 = 1.75 exactly. This is the one pin that actually
        // protects `sigma`/`KERNEL_EPSILON`: unlike the `< 5%` check above
        // (or the weight-sum check, which any normalized sigma passes by
        // construction), a wrong sigma such as `h/2` or `h/sqrt(-ln(eps))`
        // still clears `< 5%` but fails this exact ratio.
        let ratio = corner_w / center_w;
        let expect = KERNEL_EPSILON.powf(1.75);
        assert!(
            (ratio - expect).abs() < 1e-9,
            "ratio={ratio} expect={expect}"
        );
    }

    #[test]
    fn square_kernel_has_no_table() {
        assert!(kernel_table(DrizzleKernel::Square, 0.9).is_none());
    }

    // (h)
    #[test]
    fn map_drop_returns_none_when_forward_is_not_finite() {
        // A homography whose numerator and denominator both vanish at
        // (-1, -1) -- a genuine runtime 0/0 singularity of an otherwise
        // well-conditioned, fully finite and invertible matrix (det = 2) --
        // rather than a literal NaN matrix entry: `PixelMap::linear` rejects
        // that at construction time (`Linear::inverse` requires a finite
        // determinant, and a NaN entry makes the determinant NaN).
        let map = PixelMap::linear(Linear {
            kind: LinearKind::Homography,
            m: [[1.0, -1.0, 0.0], [0.0, 1.0, 0.0], [1.0, 1.0, 2.0]],
        })
        .expect("well-conditioned homography");
        let (u, _v) = map.forward(-1.0, -1.0);
        assert!(u.is_nan(), "expected a genuine 0/0 NaN, got {u}");

        let corners = drop_corners(0, 0, 2.0); // corner (x - h, y - h) = (-1, -1)
        assert_eq!(map_drop(&map, &corners, 1), None);
    }
}

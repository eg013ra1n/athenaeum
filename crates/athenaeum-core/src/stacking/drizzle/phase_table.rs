//! The per-phase drop-overlap table (Tier C item C2, spec
//! `2026-09-20-stacking-compute-tierC-design.md` §3, ruling C-3).
//!
//! What it replaces: [`super::deposit_band`] used to map each source
//! pixel's four drop corners through the frame's [`ForwardEval`]
//! ([`super::geom::map_drop`]) and then run one Sutherland–Hodgman clip
//! ([`super::geom::clip_area`], ≈ 100–120 `f64` operations with ≈ 8
//! divisions) per overlapped output pixel — ≈ 422 ns per source pixel.
//!
//! The observation C2 rests on: under a LINEAR map the mapped drop is ONE
//! parallelogram for the whole frame. Only its sub-pixel PHASE — the
//! fractional part of its mapped centre on the output grid — varies from
//! pixel to pixel, and a phase is a two-dimensional quantity with a
//! bounded range. So the overlap areas can be tabulated once: for each of
//! `PHASES × PHASES` representative phases, clip the parallelogram
//! (translated to that phase) against every output pixel its bounding box
//! touches, through the SAME [`super::geom::clip_area`] the exact path
//! calls, and store the resulting `(cell offset, area)` list. The deposit
//! then costs one floor + one table lookup + up to ~9–16 multiply-adds,
//! with no division at all.
//!
//! **Level preservation (ruling R-M3-2) holds by construction.** Every
//! phase's own area row is a partition of the SAME quad — the clips of one
//! convex polygon against the disjoint unit cells its bounding box covers
//! — so each row sums to the quad's area exactly as the per-pixel clips
//! did. `I / W` is therefore still the input level for a uniform field,
//! at every scale and `dropShrink`. Pinned by
//! `every_phase_row_sums_to_the_drops_own_area`.
//!
//! **What moves numerically.** A pixel's phase is rounded to the centre of
//! its `1 / PHASES` bin, so its area is split among the neighbouring
//! output pixels as if the drop sat at most `1 / (2 · PHASES)` = 1/64 of
//! an output pixel away from where it really is. The total deposited mass
//! is unchanged (above); only its distribution among neighbours shifts.
//! Spec §3.2 measured the resulting per-frame weight change at ≤ 1.6 %
//! (nearest) / < 0.1 % (bilinear) and the master-level change at 0.1 % /
//! 0.007 % over 208 frames; the fixture pins
//! `the_phase_table_tracks_the_exact_clip_on_a_rotated_frame` and
//! `..._on_a_tps_frame` hold the per-pixel deviation to ≤ 2 % of the local
//! value with the plane level within 1e-3 and coverage identical.
//!
//! **Under distortion** the parallelogram varies slowly across the frame,
//! so the table is rebuilt per [`TILE`]-pixel source tile from the map's
//! LOCAL Jacobian at that tile's centre ([`super::geom::map_drop_at`]).
//! Ruling C-3 fixes both numbers: `PHASES = 32`, one table per frame for
//! `distortion.is_none()` and one per 256-px tile otherwise.
//!
//! **What the table never decides.** It supplies the GEOMETRY of one
//! drop's overlap and nothing else: which source pixels are read, whether
//! a pixel is rejected (`.rej`), which colour plane a mosaic pixel routes
//! to ([`super::geom::cfa_plane_of`], M4d ruling R-M4d-2), the local
//! normalization grid, the frame weight and the output pair are all
//! exactly the exact path's and are applied by the SAME shared code —
//! [`super::deposit_band`] resolves `(cells, areas)` from one arm or the
//! other and then runs ONE accumulation loop.

use super::geom::{self, Quad};

/// The phase grid's resolution per axis (ruling C-3): a mapped drop
/// centre's fractional position is rounded to one of `PHASES` bins on each
/// axis, i.e. to within `1 / (2 · PHASES)` of an output pixel.
pub const PHASES: usize = 32;

/// The source-tile edge, in SOURCE pixels, a distortion map's table is
/// rebuilt over (ruling C-3). A tile's table is built from the map's local
/// Jacobian at the tile CENTRE, so the worst in-tile error is the map's
/// own Jacobian variation over half a tile — which for every registration
/// distortion this pipeline fits (M4c's own hold-out rms was 0.1–0.2 px
/// over a whole 26 Mpx frame) is far below the phase quantization above.
pub const TILE: usize = 256;

/// The largest cell offset, in output pixels from the floor of the mapped
/// drop centre, a table entry may carry. Comfortably above the real range
/// (`scale = 3`, `dropShrink = 1`, rotated 45° ⇒ a half-diagonal of
/// ≈ 2.1 output pixels ⇒ offsets in `-3..=3`), and chosen so every offset
/// fits an `i8` with room to spare. A quad that needs more than this is a
/// degenerate local mapping — [`PhaseTable::build`] refuses it and the
/// caller falls back to the exact per-pixel clip rather than allocating an
/// unbounded table.
const MAX_CELL_OFFSET: i64 = 60;

/// The largest number of output cells ONE phase's drop may overlap before
/// [`PhaseTable::build`] refuses (same reasoning as [`MAX_CELL_OFFSET`]:
/// bound the table, never the correctness). 16 × 16 is four times the
/// `scale = 3` worst case.
const MAX_CELLS_PER_PHASE: usize = 256;

/// One mapped drop's overlap areas, tabulated over a `PHASES × PHASES`
/// grid of sub-pixel phases.
///
/// Storage is one flat CSR-shaped pair: `starts[k] .. starts[k + 1]` is
/// phase `k = iy * PHASES + ix`'s slice of `cells` (the output-pixel
/// offsets from `floor` of the mapped centre) and of `areas` (the
/// corresponding overlap areas, in output pixels²). At `PHASES = 32` with
/// a `scale = 2` drop that is ≈ 1024 × 9 entries ≈ 55 KB — built once per
/// frame (or per [`TILE`] under distortion), never per pixel.
#[derive(Debug, Clone)]
pub struct PhaseTable {
    starts: Vec<u32>,
    cells: Vec<(i8, i8)>,
    areas: Vec<f32>,
    /// The mapped quad's own area in output pixels² — what every phase
    /// row sums to (ruling R-M3-2; see the module doc).
    drop_area: f64,
    phases: usize,
}

impl PhaseTable {
    /// Tabulates `quad_at_origin` — one drop already mapped into OUTPUT
    /// coordinates and translated so its mapped CENTRE sits at the origin,
    /// counter-clockwise (what [`geom::map_drop_at`] returns).
    ///
    /// For each phase `(ix, iy)` the quad is translated to the phase bin's
    /// own centre `((ix + 0.5) / phases, (iy + 0.5) / phases)` and clipped
    /// against every output pixel [`geom::quad_bbox`] reports — the SAME
    /// bounding-box rule and the SAME [`geom::clip_area`] the exact path
    /// uses, so a table entry is bit-for-bit the number the exact path
    /// would have computed for a drop sitting at that exact phase (pinned
    /// by `every_entry_is_the_exact_clip_of_the_translated_drop`).
    ///
    /// Cells whose clipped area is zero are not stored: the deposit skips
    /// them anyway, and dropping them shortens the hot loop.
    ///
    /// `scale` is the drizzle scale the quad was mapped with — carried for
    /// the contract assertion only (the quad is ALREADY in output units,
    /// so no scaling happens in here; a caller passing a reference-unit
    /// quad would silently tabulate a drop `scale` times too small).
    ///
    /// Returns `None` for a quad that is not finite, has no area, or is so
    /// large that it would blow [`MAX_CELL_OFFSET`] / [`MAX_CELLS_PER_PHASE`]
    /// — every one a degenerate local mapping the caller answers by
    /// falling back to the exact clip.
    pub fn build(quad_at_origin: &Quad, scale: u32, phases: usize) -> Option<PhaseTable> {
        debug_assert!(scale >= 1, "drizzle scale must be >= 1 (R-M3-10), got 0");
        debug_assert!(phases >= 1, "a phase table needs at least one bin");
        if phases == 0 {
            return None;
        }
        if quad_at_origin
            .iter()
            .any(|&(x, y)| !x.is_finite() || !y.is_finite())
        {
            return None;
        }
        let drop_area = geom::quad_area(quad_at_origin);
        if !(drop_area > 0.0) {
            return None;
        }

        let inv = 1.0 / phases as f64;
        let mut starts = Vec::with_capacity(phases * phases + 1);
        let mut cells: Vec<(i8, i8)> = Vec::new();
        let mut areas: Vec<f32> = Vec::new();
        starts.push(0u32);

        for iy in 0..phases {
            let fy = (iy as f64 + 0.5) * inv;
            for ix in 0..phases {
                let fx = (ix as f64 + 0.5) * inv;
                let mut quad = *quad_at_origin;
                for c in quad.iter_mut() {
                    c.0 += fx;
                    c.1 += fy;
                }
                let (bx0, by0, bx1, by1) = geom::quad_bbox(&quad);
                if bx0 < -MAX_CELL_OFFSET
                    || by0 < -MAX_CELL_OFFSET
                    || bx1 > MAX_CELL_OFFSET
                    || by1 > MAX_CELL_OFFSET
                {
                    return None;
                }
                let mut in_phase = 0usize;
                for py in by0..=by1 {
                    for px in bx0..=bx1 {
                        let a = geom::clip_area(&quad, px, py);
                        if a > 0.0 {
                            in_phase += 1;
                            if in_phase > MAX_CELLS_PER_PHASE {
                                return None;
                            }
                            cells.push((px as i8, py as i8));
                            areas.push(a as f32);
                        }
                    }
                }
                starts.push(cells.len() as u32);
            }
        }

        Some(PhaseTable {
            starts,
            cells,
            areas,
            drop_area,
            phases,
        })
    }

    /// The `(cells, areas)` of the phase bin containing the fractional
    /// mapped-centre position `(fx, fy)`, both expected in `[0, 1)` (the
    /// caller's `ox - ox.floor()`).
    ///
    /// Out-of-range inputs are clamped rather than refused: `as usize` on
    /// a negative `f64` saturates to 0 in Rust, and `fx == 1.0` (reachable
    /// only through a rounding artefact in the caller's `floor`) is pulled
    /// back to the last bin — a deposit must never index out of bounds
    /// over a one-ulp coordinate.
    #[inline]
    pub fn lookup(&self, fx: f64, fy: f64) -> (&[(i8, i8)], &[f32]) {
        let n = self.phases;
        let ix = ((fx * n as f64) as usize).min(n - 1);
        let iy = ((fy * n as f64) as usize).min(n - 1);
        let k = iy * n + ix;
        let lo = self.starts[k] as usize;
        let hi = self.starts[k + 1] as usize;
        (&self.cells[lo..hi], &self.areas[lo..hi])
    }

    /// The tabulated drop's own area in output pixels² — the value every
    /// phase row sums to (ruling R-M3-2).
    pub fn drop_area(&self) -> f64 {
        self.drop_area
    }

    /// Bytes this table's three vectors hold — reported by the probe so
    /// the per-frame / per-tile footprint the spec quotes (≈ 37–55 KB)
    /// stays an observed number rather than an estimate.
    pub fn heap_bytes(&self) -> usize {
        self.starts.len() * std::mem::size_of::<u32>()
            + self.cells.len() * std::mem::size_of::<(i8, i8)>()
            + self.areas.len() * std::mem::size_of::<f32>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::linear::{Linear, LinearKind};
    use crate::geometry::pixel_map::PixelMap;

    fn rotation_map(deg: f64) -> PixelMap {
        let r = deg.to_radians();
        let (s, c) = r.sin_cos();
        PixelMap::linear(Linear {
            kind: LinearKind::Affine,
            m: [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]],
        })
        .unwrap()
    }

    /// Pin (a): every stored entry is EXACTLY what `geom::clip_area`
    /// returns for the drop translated to that phase bin's own centre, and
    /// the cell offsets index the right output pixels (the whole point of
    /// the pin — the table is built from `clip_area`, so a mismatch can
    /// only come from the offsets or the CSR indexing, which is what this
    /// walks through `lookup`).
    #[test]
    fn every_entry_is_the_exact_clip_of_the_translated_drop() {
        for &deg in &[1.0_f64, 5.0, 30.0] {
            for &(scale, shrink) in &[(2u32, 0.9_f64), (2, 1.0), (3, 0.8)] {
                let map = rotation_map(deg);
                let quad = geom::map_drop_at(&map, 100.0, 100.0, shrink, scale)
                    .expect("a finite rotation maps a finite drop");
                let table = PhaseTable::build(&quad, scale, PHASES).expect("a sane drop tabulates");

                for iy in 0..PHASES {
                    for ix in 0..PHASES {
                        let fx = (ix as f64 + 0.5) / PHASES as f64;
                        let fy = (iy as f64 + 0.5) / PHASES as f64;
                        let mut translated = quad;
                        for c in translated.iter_mut() {
                            c.0 += fx;
                            c.1 += fy;
                        }
                        let (cells, areas) = table.lookup(fx, fy);
                        // Every stored cell matches the exact clip there.
                        for (&(dx, dy), &a) in cells.iter().zip(areas.iter()) {
                            let exact = geom::clip_area(&translated, dx as i64, dy as i64);
                            assert!(
                                (a as f64 - exact).abs() <= 1e-6,
                                "deg {deg} scale {scale} shrink {shrink} phase ({ix},{iy}) cell ({dx},{dy}): table {a} vs exact {exact}"
                            );
                        }
                        // And nothing with a non-zero exact area is missing:
                        // every pixel of the drop's own bbox is either stored
                        // or clips to exactly zero.
                        let (bx0, by0, bx1, by1) = geom::quad_bbox(&translated);
                        for py in by0..=by1 {
                            for px in bx0..=bx1 {
                                let exact = geom::clip_area(&translated, px, py);
                                let stored = cells
                                    .iter()
                                    .position(|&(dx, dy)| dx as i64 == px && dy as i64 == py)
                                    .map(|i| areas[i] as f64);
                                match stored {
                                    Some(a) => assert!((a - exact).abs() <= 1e-6),
                                    None => assert_eq!(
                                        exact, 0.0,
                                        "deg {deg} phase ({ix},{iy}) pixel ({px},{py}) has area {exact} but is not in the table"
                                    ),
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Pin (b): level preservation (ruling R-M3-2) — each phase's areas
    /// partition the SAME quad, so each row sums to the drop's own mapped
    /// area. This is what makes the deposit level-preserving regardless of
    /// the phase quantization: quantizing WHERE the mass goes never
    /// changes HOW MUCH of it there is.
    #[test]
    fn every_phase_row_sums_to_the_drops_own_area() {
        for &deg in &[0.0_f64, 1.0, 5.0, 30.0, 45.0] {
            for &(scale, shrink) in &[(1u32, 0.9_f64), (2, 0.9), (2, 1.0), (3, 0.8), (3, 1.0)] {
                let map = rotation_map(deg);
                let quad = geom::map_drop_at(&map, 500.0, 500.0, shrink, scale).unwrap();
                let table = PhaseTable::build(&quad, scale, PHASES).unwrap();
                let want = table.drop_area();
                // The mapped area of a rotated drop is `(shrink · scale)²`
                // — a rotation preserves area — so the invariant is pinned
                // against an INDEPENDENTLY known number too, not only
                // against the table's own bookkeeping.
                let analytic = (shrink * scale as f64).powi(2);
                assert!(
                    (want - analytic).abs() <= 1e-9,
                    "deg {deg} scale {scale} shrink {shrink}: quad area {want} != {analytic}"
                );
                for iy in 0..PHASES {
                    for ix in 0..PHASES {
                        let fx = (ix as f64 + 0.5) / PHASES as f64;
                        let fy = (iy as f64 + 0.5) / PHASES as f64;
                        let (_, areas) = table.lookup(fx, fy);
                        let sum: f64 = areas.iter().map(|&a| a as f64).sum();
                        assert!(
                            (sum - want).abs() <= 1e-6,
                            "deg {deg} scale {scale} shrink {shrink} phase ({ix},{iy}): Σ area {sum} != drop area {want}"
                        );
                    }
                }
            }
        }
    }

    /// A degenerate local mapping must fall back, not allocate: a drop
    /// mapped 200× larger than an output pixel blows [`MAX_CELL_OFFSET`]
    /// and the build refuses rather than tabulating 40 000 cells per
    /// phase.
    #[test]
    fn an_absurdly_large_drop_refuses_to_tabulate() {
        let map = PixelMap::linear(Linear {
            kind: LinearKind::Affine,
            m: [[200.0, 0.0, 0.0], [0.0, 200.0, 0.0], [0.0, 0.0, 1.0]],
        })
        .unwrap();
        let quad = geom::map_drop_at(&map, 10.0, 10.0, 1.0, 1).unwrap();
        assert!(
            PhaseTable::build(&quad, 1, PHASES).is_none(),
            "a 200-output-pixel drop must refuse the table, not build one"
        );
    }

    /// A collapsed quad (`dropShrink = 0`, or a singular local Jacobian)
    /// has no area to distribute — refused, so the caller keeps the exact
    /// path which already handles it by depositing nothing.
    #[test]
    fn a_zero_area_drop_refuses_to_tabulate() {
        let map = rotation_map(0.0);
        let quad = geom::map_drop_at(&map, 10.0, 10.0, 0.0, 2).unwrap();
        assert!(PhaseTable::build(&quad, 2, PHASES).is_none());
    }
}

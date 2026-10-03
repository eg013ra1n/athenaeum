//! The canonical grid inside a project run (spec §5.5, §5.9; plan 6b-3 D1,
//! D2, D11): the grid's TAN, a reference frame's WCS, the composed subject →
//! grid map every pixel consumer reads, the hash a grid-registered frame is
//! cached under, and the synthetic WCS a grid master carries.
//!
//! The composed map is ONE `PixelMap`. Its affine part is fitted to the
//! exact chain subject → reference (the star map) → sky (the reference's
//! WCS, SIP included) → grid (pure TAN), sampled every
//! [`COMPOSE_SAMPLE_PX`] including the last row and column. Its distortion
//! layer is a [`DistortionModel::Sampled`] over [`ComposedField`]: the exact
//! path evaluates the chain (the inverse by Newton steps on the forward
//! chain, never SIP's reverse tables or the star map's own fitted inverse);
//! the pixel path reads bilinear grids built lazily from it.
//!
//! Orientation: every solve in the owner's catalog has det CD > 0 and the
//! grid is sky-standard (det CD < 0), so a composed map is a reflection
//! (`PixelMap::is_flipped`) and a grid master is the mirror image of a raw
//! frame — correct on the sky.

use std::path::Path;
use std::sync::Arc;

use solvemyastro::wcs::WcsSolution;

use crate::collab::grid::CanonicalGrid;
use crate::collab::grid_geometry::grid_wcs;
use crate::geometry::linear::{fit_affine, Linear, Pair};
use crate::geometry::{DisplacementField, DistortionModel, PixelMap};
use crate::plate_solve::storage::PlateSolveRecord;
use crate::stacking::config::stage_hash;

/// Spacing (px) of the chain samples the affine part is fitted on (spec §5.5).
pub const COMPOSE_SAMPLE_PX: usize = 32;

/// Bumped when the composition's arithmetic changes: it is folded into every
/// grid-registered frame's hash, so a bump rewrites them.
pub const COMPOSE_VERSION: u32 = 1;

/// Newton iterations of the inverse chain, and its convergence tolerance.
/// 1e-8 px is reachable at every pixel scale (the chain's own rounding is
/// ~1e-10 px at 2000 px coordinates), so a converging point stops after a
/// few steps instead of spending all [`INVERSE_MAX_ITER`].
const INVERSE_MAX_ITER: usize = 30;
const INVERSE_TOL_PX: f64 = 1e-8;

/// A point whose Newton iteration ends above this residual (px) did not
/// converge: it is off the footprint (`NaN`), never an unconverged iterate
/// that would map a grid pixel outside the frame into it.
const INVERSE_FAIL_PX: f64 = 1e-6;

/// How far (px) the sampled domain reaches beyond the footprint: enough
/// for any interpolation kernel's support at the frame's edge.
const DOMAIN_MARGIN_PX: f64 = 64.0;

/// What a pixel whose chain cannot be evaluated is displaced by: far
/// outside every frame, so every consumer reads NaN there, never pixel 0.
const OFF_FOOTPRINT_PX: f64 = 1.0e7;

/// A project run's grid: the confirmed grid, its hash (`ATH_GRID`) and its
/// TAN projection.
#[derive(Debug, Clone)]
pub struct GridRun {
    pub grid: CanonicalGrid,
    pub hash: String,
    pub tan: WcsSolution,
}

impl GridRun {
    pub fn new(grid: CanonicalGrid) -> GridRun {
        let grid = grid.normalized();
        GridRun {
            hash: grid.hash(),
            tan: grid_wcs(&grid),
            grid,
        }
    }

    /// `(width, height)` in pixels — the geometry every group of the run is
    /// delivered in.
    pub fn dims(&self) -> (usize, usize) {
        (self.grid.width_px as usize, self.grid.height_px as usize)
    }
}

/// A registration reference's WCS (manifest or local solve), with the hash
/// of its geometry.
#[derive(Debug, Clone)]
pub struct ReferenceWcs {
    pub solution: WcsSolution,
    pub hash: String,
}

impl ReferenceWcs {
    /// `None` when the record's SIP tables cannot be read
    /// (`PlateSolveRecord::to_solution` logs why).
    pub fn from_record(record: &PlateSolveRecord) -> Option<ReferenceWcs> {
        let solution = record.to_solution()?;
        Some(ReferenceWcs {
            hash: wcs_record_hash(record),
            solution,
        })
    }
}

/// XXH3-64 (16 hex) of a solve's GEOMETRY — CRPIX, CRVAL, CD and the SIP
/// tables — never its metadata (star counts, times), so a re-solve that
/// moved nothing keeps every grid-registered frame.
pub fn wcs_record_hash(r: &PlateSolveRecord) -> String {
    let text = format!(
        "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{}|{}|{}|{}",
        r.crpix1,
        r.crpix2,
        r.crval1,
        r.crval2,
        r.cd1_1,
        r.cd1_2,
        r.cd2_1,
        r.cd2_2,
        r.sip_order,
        r.sip_a_coeffs.as_deref().unwrap_or(""),
        r.sip_b_coeffs.as_deref().unwrap_or(""),
        r.sip_ap_coeffs.as_deref().unwrap_or(""),
        r.sip_bp_coeffs.as_deref().unwrap_or(""),
    );
    format!("{:016x}", xxhash_rust::xxh3::xxh3_64(text.as_bytes()))
}

/// The WCS geometry hash of a registration reference — its manifest WCS,
/// else the run's local solve of it (`origin::stored_solve`) — or `None`
/// when it has neither. The plan and the run key a grid-registered frame on
/// it (plan 6b-3 D2).
pub fn reference_wcs_hash_of(
    conn: &rusqlite::Connection,
    origins: &crate::stacking::origin::FrameOrigins,
    frame_id: i64,
) -> anyhow::Result<Option<String>> {
    let origin =
        crate::stacking::origin::origin_of(origins, frame_id).map_err(anyhow::Error::from)?;
    Ok(
        crate::stacking::origin::stored_solve(conn, origin, frame_id)?
            .map(|record| wcs_record_hash(&record)),
    )
}

/// The exact chain subject → grid, as a [`DisplacementField`] around the
/// fitted affine `linear`.
///
/// Wherever the star map clamps its query at a domain edge (a spline's
/// domain, or a polynomial's `fitted_domain`: the star bounding box plus
/// 10 %), the field has a derivative kink on that edge's image. The edge
/// can lie INSIDE the frame, so the kink is not confined to the frame's
/// rim: the one 8-px bilinear cell straddling it carries up to about
/// `h · |Δslope| / 4` of error (`h` = [`TPS_GRID_PX`], `Δslope` the jump of
/// the field's derivative across the edge). The 0.01 px pin is a statement
/// about everywhere else; the kink line has its own, documented bound
/// (`a_clamp_kink_inside_the_frame_stays_within_its_documented_bound`).
/// Do not tighten the pin at the kink.
#[derive(Debug)]
pub struct ComposedField {
    star: PixelMap,
    reference: WcsSolution,
    grid: WcsSolution,
    linear: Linear,
    linear_inv: Linear,
    key: String,
}

impl ComposedField {
    /// Subject pixel → grid pixel, exactly: the star map's exact path, the
    /// reference's WCS forward (SIP A/B), the grid's TAN backward.
    pub fn chain(&self, x: f64, y: f64) -> (f64, f64) {
        chain_of(&self.star, &self.reference, &self.grid, x, y)
    }

    /// The subject pixel whose chain lands on `(gx, gy)`: Newton steps from
    /// the affine's own inverse, with the affine's Jacobian. The residual
    /// the affine leaves is a smooth few-pixel field, so this contracts by
    /// orders of magnitude per step. `NaN` when the chain is undefined, and
    /// `NaN` when the iteration ends above [`INVERSE_FAIL_PX`]: a point that
    /// does not converge is off the footprint.
    pub fn chain_inverse(&self, gx: f64, gy: f64) -> (f64, f64) {
        let (mut sx, mut sy) = self.linear_inv.apply(gx, gy);
        let m = &self.linear_inv.m;
        let mut residual = f64::INFINITY;
        for iter in 0..=INVERSE_MAX_ITER {
            let (cx, cy) = self.chain(sx, sy);
            let (rx, ry) = (gx - cx, gy - cy);
            if !(rx.is_finite() && ry.is_finite()) {
                return (f64::NAN, f64::NAN);
            }
            residual = rx.abs().max(ry.abs());
            if residual <= INVERSE_TOL_PX || iter == INVERSE_MAX_ITER {
                break;
            }
            sx += m[0][0] * rx + m[0][1] * ry;
            sy += m[1][0] * rx + m[1][1] * ry;
        }
        if residual > INVERSE_FAIL_PX {
            return (f64::NAN, f64::NAN);
        }
        (sx, sy)
    }
}

fn chain_of(
    star: &PixelMap,
    reference: &WcsSolution,
    grid: &WcsSolution,
    x: f64,
    y: f64,
) -> (f64, f64) {
    let (rx, ry) = star.forward_exact(x, y);
    let (ra, dec) = reference.pixel_to_sky(rx, ry);
    grid.sky_to_pixel(ra, dec)
}

fn finite_or_off(d: (f64, f64)) -> (f64, f64) {
    if d.0.is_finite() && d.1.is_finite() {
        d
    } else {
        (OFF_FOOTPRINT_PX, OFF_FOOTPRINT_PX)
    }
}

impl DisplacementField for ComposedField {
    fn forward(&self, x: f64, y: f64) -> (f64, f64) {
        let (sx, sy) = self.linear_inv.apply(x, y);
        let (gx, gy) = self.chain(sx, sy);
        finite_or_off((gx - x, gy - y))
    }

    fn inverse(&self, x: f64, y: f64) -> (f64, f64) {
        let (sx, sy) = self.chain_inverse(x, y);
        let (lx, ly) = self.linear.apply(sx, sy);
        finite_or_off((lx - x, ly - y))
    }

    fn key(&self) -> &str {
        &self.key
    }
}

/// `0, 32, 64, …` up to `n − 1`, the last pixel always included.
fn lattice(n: usize) -> Vec<f64> {
    let mut out: Vec<f64> = (0..n)
        .step_by(COMPOSE_SAMPLE_PX)
        .map(|v| v as f64)
        .collect();
    let last = (n - 1) as f64;
    if out.last() != Some(&last) {
        out.push(last);
    }
    out
}

/// The composed subject → grid map of one frame (spec §5.5, plan 6b-3 D1):
/// `star` is the frame's registration onto `reference`; `subject_dims` is
/// the frame's own `(width, height)`. `Err` names why it cannot be built —
/// a frame with no pixels, a chain undefined on the frame, a singular
/// affine, or an inverse chain that does not converge — and the caller
/// excludes the frame with that reason.
pub fn compose_to_grid(
    star: &PixelMap,
    subject_dims: (usize, usize),
    reference: &ReferenceWcs,
    grid: &GridRun,
) -> Result<PixelMap, String> {
    let (field, domain) = compose_field(star, subject_dims, reference, grid)?;
    let linear = field.linear;
    PixelMap::with_distortion_model(linear, DistortionModel::sampled(Arc::new(field), domain))
        .ok_or_else(|| "the grid affine is singular".to_string())
}

/// The field and the sampled domain behind [`compose_to_grid`].
fn compose_field(
    star: &PixelMap,
    subject_dims: (usize, usize),
    reference: &ReferenceWcs,
    grid: &GridRun,
) -> Result<(ComposedField, [f64; 4]), String> {
    let (w, h) = subject_dims;
    if w == 0 || h == 0 {
        return Err("the frame has no pixels".to_string());
    }
    if matches!(star.distortion, Some(DistortionModel::Sampled { .. })) {
        return Err("a composed map cannot be composed again".to_string());
    }
    let (xs, ys) = (lattice(w), lattice(h));
    let mut pairs: Vec<Pair> = Vec::with_capacity(xs.len() * ys.len());
    for &y in &ys {
        for &x in &xs {
            let g = chain_of(star, &reference.solution, &grid.tan, x, y);
            if !(g.0.is_finite() && g.1.is_finite()) {
                return Err(format!(
                    "the grid chain is undefined at frame pixel ({x}, {y})"
                ));
            }
            pairs.push(((x, y), g));
        }
    }
    let linear = fit_affine(&pairs, None).ok_or("the grid affine is singular")?;
    let linear_inv = linear.inverse().ok_or("the grid affine is singular")?;

    let mut b = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for ((x, y), g) in &pairs {
        for (px, py) in [linear.apply(*x, *y), *g] {
            b[0] = b[0].min(px);
            b[1] = b[1].min(py);
            b[2] = b[2].max(px);
            b[3] = b[3].max(py);
        }
    }
    let domain = [
        b[0] - DOMAIN_MARGIN_PX,
        b[1] - DOMAIN_MARGIN_PX,
        b[2] + DOMAIN_MARGIN_PX,
        b[3] + DOMAIN_MARGIN_PX,
    ];
    let key = format!(
        "{:016x}",
        xxhash_rust::xxh3::xxh3_64(
            format!(
                "{}|{}|{}|{w}x{h}|{COMPOSE_VERSION}",
                star.to_json(),
                reference.hash,
                grid.hash
            )
            .as_bytes()
        )
    );
    let field = ComposedField {
        star: star.clone(),
        reference: reference.solution.clone(),
        grid: grid.tan.clone(),
        linear,
        linear_inv,
        key,
    };
    let (wl, hl) = ((w - 1) as f64, (h - 1) as f64);
    for (x, y) in [
        (0.0, 0.0),
        (wl, 0.0),
        (0.0, hl),
        (wl, hl),
        (wl / 2.0, hl / 2.0),
    ] {
        let g = field.chain(x, y);
        let (bx, by) = field.chain_inverse(g.0, g.1);
        if !((bx - x).abs() <= 1e-6 && (by - y).abs() <= 1e-6) {
            return Err(format!(
                "the inverse grid chain does not converge at frame pixel ({x}, {y})"
            ));
        }
    }
    Ok((field, domain))
}

/// The hash a grid-registered frame's `registered` artifact is cached under
/// (spec §5.5, plan 6b-3 D2): today's registration hash (the star map), the
/// reference's WCS geometry and the grid, with [`COMPOSE_VERSION`].
pub fn grid_registered_hash(
    registration_hash: &str,
    reference_wcs_hash: &str,
    grid_hash: &str,
) -> String {
    stage_hash(
        &serde_json::json!({ "grid": grid_hash, "composeVersion": COMPOSE_VERSION }),
        &[registration_hash, &format!("refwcs:{reference_wcs_hash}")],
        &[],
    )
}

/// The synthetic plate solve a grid master carries (spec §5.9, plan 6b-3
/// D11): the grid's TAN with a 0-based CRPIX at the grid's centre (the card
/// writer adds 1), no SIP, `"grid"` as catalog and algorithm.
pub fn grid_plate_solve(grid: &CanonicalGrid, solved_at: &str) -> PlateSolveRecord {
    let g = grid.normalized();
    let tan = grid_wcs(&g);
    PlateSolveRecord {
        id: None,
        frame_id: 0,
        crpix1: tan.crpix.0,
        crpix2: tan.crpix.1,
        crval1: tan.crval.0,
        crval2: tan.crval.1,
        cd1_1: tan.cd[0][0],
        cd1_2: tan.cd[0][1],
        cd2_1: tan.cd[1][0],
        cd2_2: tan.cd[1][1],
        sip_order: None,
        sip_a_coeffs: None,
        sip_b_coeffs: None,
        sip_ap_coeffs: None,
        sip_bp_coeffs: None,
        matched_stars: 0,
        total_detected: 0,
        rms_residual_px: 0.0,
        rms_residual_arcsec: 0.0,
        pixel_scale_arcsec: g.scale_arcsec,
        field_rotation_deg: tan.field_rotation_deg(),
        solve_time_ms: 0,
        catalog_used: "grid".to_string(),
        algorithm_used: "grid".to_string(),
        solved_at: solved_at.to_string(),
        expected_catalog_stars_in_fov: None,
        inlier_ratio: None,
    }
}

/// `record` with CRPIX moved by a crop's origin. SIP terms are in pixel
/// offsets from CRPIX, so they need no change; the result is exact for a
/// frame-set master's SIP solve and a grid's TAN alike.
pub fn crop_plate_solve(record: &PlateSolveRecord, x0: usize, y0: usize) -> PlateSolveRecord {
    let mut out = record.clone();
    out.crpix1 -= x0 as f64;
    out.crpix2 -= y0 as f64;
    out
}

/// A frame's own `(width, height)`: the frame source's when it has one,
/// else the file's header.
pub fn subject_dims(width: i64, height: i64, path: &Path) -> Result<(usize, usize), String> {
    if width > 0 && height > 0 {
        return Ok((width as usize, height as usize));
    }
    let reader = crate::integration::plane_reader::PlaneReader::open(path)
        .map_err(|e| format!("reading {}: {e}", path.display()))?;
    Ok((reader.width(), reader.height()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::CardValue;
    use crate::geometry::linear::LinearKind;
    use crate::geometry::pixel_map::grid_counters;
    use crate::geometry::{Distortion, Polynomial2D, ThinPlateSpline};

    const W: usize = 1600;
    const H: usize = 1200;
    const SCALE: f64 = 1.87;
    /// Cubic SIP terms: ≈ 10 px of non-affine displacement at this frame's
    /// corners, the size research §3.6 measured on the owner's rigs.
    const K3: f64 = 2.0e-8;

    /// A reference solve in the owner's parity (det CD > 0, every stored
    /// solve), unrotated, with SIP order 3.
    fn reference_record(ra: f64, dec: f64) -> PlateSolveRecord {
        let s = SCALE / 3600.0;
        let a = vec![
            vec![0.0, 0.0, 0.0, 0.0],
            vec![0.0, 0.0, K3],
            vec![0.0, 0.0],
            vec![K3],
        ];
        let b = vec![
            vec![0.0, 0.0, 0.0, K3],
            vec![0.0, 0.0, 0.0],
            vec![0.0, K3],
            vec![0.0],
        ];
        PlateSolveRecord {
            id: None,
            frame_id: 0,
            crpix1: (W as f64 - 1.0) / 2.0,
            crpix2: (H as f64 - 1.0) / 2.0,
            crval1: ra,
            crval2: dec,
            cd1_1: s,
            cd1_2: 0.0,
            cd2_1: 0.0,
            cd2_2: s,
            sip_order: Some(3),
            sip_a_coeffs: Some(serde_json::to_string(&a).unwrap()),
            sip_b_coeffs: Some(serde_json::to_string(&b).unwrap()),
            sip_ap_coeffs: None,
            sip_bp_coeffs: None,
            matched_stars: 100,
            total_detected: 200,
            rms_residual_px: 0.1,
            rms_residual_arcsec: 0.2,
            pixel_scale_arcsec: SCALE,
            field_rotation_deg: 0.0,
            solve_time_ms: 1,
            catalog_used: "test".into(),
            algorithm_used: "test".into(),
            solved_at: "2026-10-03T00:00:00Z".into(),
            expected_catalog_stars_in_fov: None,
            inlier_ratio: None,
        }
    }

    fn exact_chain(
        star: &PixelMap,
        reference: &ReferenceWcs,
        grid: &GridRun,
        x: f64,
        y: f64,
    ) -> (f64, f64) {
        let (rx, ry) = star.forward_exact(x, y);
        let (ra, dec) = reference.solution.pixel_to_sky(rx, ry);
        grid.tan.sky_to_pixel(ra, dec)
    }

    /// Composes `star` onto a grid `offset_deg` north of the reference's
    /// centre and checks the PIXEL path (the bilinear grids every consumer
    /// reads) against the exact chain at every 32-px cell centre, both
    /// directions, and the round trip.
    fn assert_pin(star: &PixelMap, offset_deg: f64) {
        let (ra, dec) = (83.8, -5.4);
        let reference = ReferenceWcs::from_record(&reference_record(ra, dec)).unwrap();
        let grid = GridRun::new(CanonicalGrid {
            scale_arcsec: SCALE,
            width_px: 2048,
            height_px: 1600,
            crval1: ra,
            crval2: dec + offset_deg,
            rotation_deg: 0.0,
        });
        let map = compose_to_grid(star, (W, H), &reference, &grid).unwrap();
        assert!(
            map.is_flipped(),
            "a det CD > 0 frame lands mirrored on the sky-standard grid"
        );
        let (mut fwd, mut back, mut round) = (0f64, 0f64, 0f64);
        let mut y = 16.0;
        while y < H as f64 {
            let mut x = 16.0;
            while x < W as f64 {
                let exact = exact_chain(star, &reference, &grid, x, y);
                let (gx, gy) = map.forward(x, y);
                fwd = fwd.max((gx - exact.0).hypot(gy - exact.1));
                let (sx, sy) = map.inverse(exact.0, exact.1);
                back = back.max((sx - x).hypot(sy - y));
                let (rx, ry) = map.inverse(gx, gy);
                round = round.max((rx - x).hypot(ry - y));
                x += 32.0;
            }
            y += 32.0;
        }
        assert!(
            fwd < 0.01,
            "forward pixel path vs the chain at {offset_deg}°: {fwd} px"
        );
        assert!(
            back < 0.01,
            "inverse pixel path vs the chain at {offset_deg}°: {back} px"
        );
        assert!(
            round < 0.01,
            "inverse(forward(s)) vs s at {offset_deg}°: {round} px"
        );
        map.release_grids();
    }

    /// Spec §5.5, plan 6b-3 D1: the composed map meets the exact chain
    /// within 0.01 px in both directions for every kind of star map the
    /// registration can produce, at the tangent point and 4° from it.
    #[test]
    fn the_composed_map_meets_the_chain_within_a_hundredth() {
        let _quiet = grid_counters::exclusive();
        let identity = PixelMap::linear(Linear::identity()).unwrap();
        let similarity = PixelMap::linear(Linear {
            kind: LinearKind::Similarity,
            m: [
                [0.9998, -0.0175, 12.3],
                [0.0175, 0.9998, -7.8],
                [0.0, 0.0, 1.0],
            ],
        })
        .unwrap();
        let homography = PixelMap::linear(Linear {
            kind: LinearKind::Homography,
            m: [[1.0, 0.002, 5.0], [-0.001, 1.0, 3.0], [1e-7, -2e-7, 1.0]],
        })
        .unwrap();
        // A cubic star-map distortion: index 3 is the u³ term of order 3.
        let mut ax = vec![0.0; 7];
        let mut ay = vec![0.0; 7];
        ax[3] = 0.6;
        ay[6] = -0.4;
        let polynomial = PixelMap::with_distortion(
            Linear::identity(),
            Distortion {
                order: 3,
                center: (W as f64 / 2.0, H as f64 / 2.0),
                scale: W as f64 / 2.0,
                domain: None,
                forward: Polynomial2D {
                    order: 3,
                    ax: ax.clone(),
                    ay: ay.clone(),
                },
                inverse: Polynomial2D {
                    order: 3,
                    ax: ax.iter().map(|v| -v).collect(),
                    ay: ay.iter().map(|v| -v).collect(),
                },
            },
        )
        .unwrap();
        let nodes: Vec<(f64, f64)> = (0..6)
            .flat_map(|i| (0..5).map(move |j| (i as f64 * 300.0 + 50.0, j as f64 * 270.0 + 40.0)))
            .collect();
        let dx: Vec<f64> = nodes
            .iter()
            .map(|(x, y)| 1.2 * (x / 400.0).sin() * (y / 350.0).cos())
            .collect();
        let dy: Vec<f64> = nodes
            .iter()
            .map(|(x, y)| 0.9 * (x / 380.0).cos() * (y / 300.0).sin())
            .collect();
        let ix: Vec<f64> = dx.iter().map(|v| -v).collect();
        let iy: Vec<f64> = dy.iter().map(|v| -v).collect();
        let spline = PixelMap::with_distortion_model(
            Linear::identity(),
            DistortionModel::tps(
                ThinPlateSpline::fit(&nodes, &dx, &dy, 0.0).unwrap(),
                ThinPlateSpline::fit(&nodes, &ix, &iy, 0.0).unwrap(),
                [0.0, 0.0, W as f64, H as f64],
            ),
        )
        .unwrap();
        for star in [&identity, &similarity, &homography, &polynomial, &spline] {
            for offset in [0.0, 4.0] {
                assert_pin(star, offset);
            }
        }
    }

    /// Spec §5.9, plan 6b-3 D11: a grid master's WCS is the grid's TAN —
    /// 0-based CRPIX at the centre (the card adds 1), sky-standard parity,
    /// the rotation read back; a crop moves CRPIX by its origin only, and
    /// drizzle's scaling commutes with a crop at twice the origin.
    #[test]
    fn the_grid_master_wcs_is_the_grids_tan() {
        let grid = CanonicalGrid {
            scale_arcsec: 2.0,
            width_px: 1024,
            height_px: 768,
            crval1: 10.5,
            crval2: 41.25,
            rotation_deg: -30.0,
        };
        let rec = grid_plate_solve(&grid, "2026-10-03T00:00:00Z");
        assert_eq!((rec.crpix1, rec.crpix2), (511.5, 383.5));
        assert_eq!(rec.sip_order, None);
        assert_eq!(rec.catalog_used, "grid");
        let sol = rec.to_solution().unwrap();
        let (ra, dec) = sol.pixel_to_sky(511.5, 383.5);
        assert!((ra - 10.5).abs() < 1e-12 && (dec - 41.25).abs() < 1e-12);
        assert!((sol.field_rotation_deg() - 330.0).abs() < 1e-9);
        assert!((sol.pixel_scale_arcsec() - 2.0).abs() < 1e-9);
        assert!(sol.cd[0][0] * sol.cd[1][1] - sol.cd[0][1] * sol.cd[1][0] < 0.0);
        let cards = crate::fits_writer::wcs::wcs_cards(&rec).unwrap();
        let crpix1 = cards.iter().find(|c| c.keyword == "CRPIX1").unwrap();
        assert_eq!(crpix1.value, Some(CardValue::Real(512.5)));

        let cropped = crop_plate_solve(&rec, 100, 50);
        let a = rec.to_solution().unwrap().pixel_to_sky(300.0, 200.0);
        let b = cropped.to_solution().unwrap().pixel_to_sky(200.0, 150.0);
        assert!((a.0 - b.0).abs() < 1e-12 && (a.1 - b.1).abs() < 1e-12);
        let scaled_after = crate::fits_writer::wcs::scale_plate_solve(&cropped, 2).unwrap();
        let scaled_before = crop_plate_solve(
            &crate::fits_writer::wcs::scale_plate_solve(&rec, 2).unwrap(),
            200,
            100,
        );
        assert!((scaled_after.crpix1 - scaled_before.crpix1).abs() < 1e-12);
        assert!((scaled_after.crpix2 - scaled_before.crpix2).abs() < 1e-12);
    }

    /// Plan 6b-3 D2: the registered hash moves with each input; the WCS hash
    /// is the reference's geometry, not its solve metadata.
    #[test]
    fn the_registered_hash_moves_with_the_star_map_the_reference_wcs_and_the_grid() {
        let base = grid_registered_hash("reg", "wcs", "grid");
        assert_eq!(base.len(), 16);
        assert_eq!(base, grid_registered_hash("reg", "wcs", "grid"));
        for other in [
            grid_registered_hash("reg2", "wcs", "grid"),
            grid_registered_hash("reg", "wcs2", "grid"),
            grid_registered_hash("reg", "wcs", "grid2"),
        ] {
            assert_ne!(base, other);
        }
        let mut r = reference_record(83.8, -5.4);
        let h = wcs_record_hash(&r);
        r.matched_stars = 7;
        r.solved_at = "later".into();
        assert_eq!(h, wcs_record_hash(&r), "solve metadata is not geometry");
        r.crpix1 += 0.5;
        assert_ne!(h, wcs_record_hash(&r));
    }

    /// A frame with no pixels, or a reference solve whose SIP cannot be
    /// read, never composes.
    #[test]
    fn composition_refuses_what_it_cannot_compose() {
        let reference = ReferenceWcs::from_record(&reference_record(83.8, -5.4)).unwrap();
        let grid = GridRun::new(CanonicalGrid {
            scale_arcsec: SCALE,
            width_px: 2048,
            height_px: 1600,
            crval1: 83.8,
            crval2: -5.4,
            rotation_deg: 0.0,
        });
        let identity = PixelMap::linear(Linear::identity()).unwrap();
        assert!(compose_to_grid(&identity, (0, H), &reference, &grid).is_err());
        let mut broken = reference_record(83.8, -5.4);
        broken.sip_b_coeffs = None;
        assert!(ReferenceWcs::from_record(&broken).is_none());
        assert_eq!(
            subject_dims(64, 48, Path::new("/nowhere")).unwrap(),
            (64, 48)
        );
        assert!(subject_dims(0, 48, Path::new("/nowhere/x.fits")).is_err());
    }

    /// A cubic star-map distortion on the frame's own centre and scale, with
    /// the given clamp domain (normalized coordinates).
    fn cubic_star(domain: Option<[f64; 4]>) -> PixelMap {
        let mut ax = vec![0.0; 7];
        let mut ay = vec![0.0; 7];
        ax[3] = 0.6;
        ay[6] = -0.4;
        PixelMap::with_distortion(
            Linear::identity(),
            Distortion {
                order: 3,
                center: (W as f64 / 2.0, H as f64 / 2.0),
                scale: W as f64 / 2.0,
                domain,
                forward: Polynomial2D {
                    order: 3,
                    ax: ax.clone(),
                    ay: ay.clone(),
                },
                inverse: Polynomial2D {
                    order: 3,
                    ax: ax.iter().map(|v| -v).collect(),
                    ay: ay.iter().map(|v| -v).collect(),
                },
            },
        )
        .unwrap()
    }

    fn test_grid(rotation_deg: f64) -> GridRun {
        GridRun::new(CanonicalGrid {
            scale_arcsec: SCALE,
            width_px: 2048,
            height_px: 1600,
            crval1: 83.8,
            crval2: -5.4,
            rotation_deg,
        })
    }

    /// Fix round 1 (M4): a composed map is not a star map.
    #[test]
    fn a_composed_map_cannot_be_composed_again() {
        let _quiet = grid_counters::exclusive();
        let reference = ReferenceWcs::from_record(&reference_record(83.8, -5.4)).unwrap();
        let grid = test_grid(0.0);
        let identity = PixelMap::linear(Linear::identity()).unwrap();
        let composed = compose_to_grid(&identity, (W, H), &reference, &grid).unwrap();
        let err = compose_to_grid(&composed, (W, H), &reference, &grid).unwrap_err();
        assert_eq!(err, "a composed map cannot be composed again");
    }

    /// Fix round 1 (M2): the inverse chain never returns an unconverged
    /// iterate. Far outside the frame, with an unbounded cubic star map, a
    /// grid pixel either has a subject pixel whose chain lands on it, or it
    /// is `NaN` (off the footprint) — never a ghost point inside the frame.
    #[test]
    fn an_unconverged_inverse_is_off_the_footprint() {
        let _quiet = grid_counters::exclusive();
        let reference = ReferenceWcs::from_record(&reference_record(83.8, -5.4)).unwrap();
        let grid = test_grid(30.0);
        let (field, _) = compose_field(&cubic_star(None), (W, H), &reference, &grid).unwrap();
        let mut nan = 0;
        let mut converged = 0;
        for i in -12..=12 {
            for j in -12..=12 {
                let (gx, gy) = (1024.0 + i as f64 * 900.0, 800.0 + j as f64 * 900.0);
                let (sx, sy) = field.chain_inverse(gx, gy);
                if sx.is_nan() || sy.is_nan() {
                    nan += 1;
                    continue;
                }
                let (cx, cy) = field.chain(sx, sy);
                assert!(
                    (cx - gx).abs().max((cy - gy).abs()) <= 1e-6,
                    "an unconverged iterate escaped at ({gx}, {gy})"
                );
                converged += 1;
            }
        }
        assert!(nan > 0, "the sweep never left the footprint");
        assert!(converged > 0, "the sweep never converged");
    }

    /// Fix round 1 (M3): a star map's clamp domain can lie INSIDE the frame,
    /// and the composed field then has a derivative kink on its image.
    /// The bilinear grid errs by at most about `h · |Δslope| / 4` in the one
    /// cell straddling it; the pin everywhere else is unchanged (0.01 px).
    #[test]
    fn a_clamp_kink_inside_the_frame_stays_within_its_documented_bound() {
        let _quiet = grid_counters::exclusive();
        let reference = ReferenceWcs::from_record(&reference_record(83.8, -5.4)).unwrap();
        let grid = test_grid(30.0);
        // Normalized domain ±0.5 around the centre: x in [400, 1200],
        // y in [200, 1000] in frame pixels.
        let star = cubic_star(Some([-0.5, -0.5, 0.5, 0.5]));
        let map = compose_to_grid(&star, (W, H), &reference, &grid).unwrap();
        let mut worst_kink = 0f64;
        let mut worst_slope = 0f64;
        for (vertical, edge) in [
            (true, 400.0),
            (true, 1200.0),
            (false, 200.0),
            (false, 1000.0),
        ] {
            let at = |t: f64, along: f64| {
                if vertical {
                    (t, along)
                } else {
                    (along, t)
                }
            };
            for k in 0..40 {
                let along = if vertical {
                    300.0 + k as f64 * 17.3
                } else {
                    450.0 + k as f64 * 17.3
                };
                for d in -80..=80 {
                    let (x, y) = at(edge + d as f64 * 0.1, along);
                    let exact = exact_chain(&star, &reference, &grid, x, y);
                    let (gx, gy) = map.forward(x, y);
                    worst_kink = worst_kink.max((gx - exact.0).hypot(gy - exact.1));
                }
                let (xa, ya) = at(edge - 0.01, along);
                let (xb, yb) = at(edge - 0.02, along);
                let (xc, yc) = at(edge + 0.02, along);
                let (xd, yd) = at(edge + 0.01, along);
                let left = {
                    let (a, b) = (
                        exact_chain(&star, &reference, &grid, xa, ya),
                        exact_chain(&star, &reference, &grid, xb, yb),
                    );
                    ((a.0 - b.0) / 0.01, (a.1 - b.1) / 0.01)
                };
                let right = {
                    let (a, b) = (
                        exact_chain(&star, &reference, &grid, xc, yc),
                        exact_chain(&star, &reference, &grid, xd, yd),
                    );
                    ((a.0 - b.0) / 0.01, (a.1 - b.1) / 0.01)
                };
                worst_slope = worst_slope.max((left.0 - right.0).hypot(left.1 - right.1));
            }
        }
        let bound = crate::geometry::TPS_GRID_PX as f64 * worst_slope / 4.0;
        assert!(
            worst_kink <= bound * 1.25,
            "kink line: {worst_kink} px exceeds 1.25 * h*|dslope|/4 = {bound}"
        );
        map.release_grids();
    }
}

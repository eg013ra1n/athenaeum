//! ASTAP square-spiral sky-cell generator.
//!
//! ASTAP searches sky position as a square spiral around a center, one
//! field-of-view per step (`unit_command_line_solving.pas:2482`
//! `STEP_SIZE:=search_field`; turning-point logic `:2506-2510`;
//! `until spiral_x>max_distance` with `max_distance≈radius/fov`). The
//! declination step is `step·spiral_y`; the RA step is divided by
//! `cos(dec)` so cells stay roughly equidistant on the sphere. Cells are
//! masked to a circular search field (square-corner rejection).
//!
//! Hinted: `center=Some` with a small radius ⇒ one (or few) cells around
//! the prior. Blind: `center=None` ⇒ full-sky spiral from (0,0), radius
//! 180° (ASTAP `radius_search1='180'`).

use crate::coordinates::angular_distance;

/// Full-sky search radius when no positional prior (ASTAP default 180°).
const FULL_SKY_RADIUS_DEG: f64 = 180.0;

/// Generate the sky-cell centers `(ra_deg, dec_deg)` to try, spiral order
/// (center first).
///
/// * `center` — positional prior; `None` ⇒ blind full-sky.
/// * `radius_deg` — search radius about the prior (ignored when blind).
/// * `step_deg` — one FOV (the current ladder rung's long-axis FOV).
pub fn spiral_cells(
    center: Option<(f64, f64)>,
    radius_deg: f64,
    step_deg: f64,
) -> Vec<(f64, f64)> {
    let (ra0, dec0) = center.unwrap_or((0.0, 0.0));
    let radius = if center.is_some() {
        radius_deg.max(0.0)
    } else {
        FULL_SKY_RADIUS_DEG
    };
    let step = step_deg.max(1e-6);
    // ASTAP: max_distance = round(radius / (fov + 1e-5)).
    let max_distance = ((radius / (step + 1e-5)).round() as i64).max(0);

    let mut out: Vec<(f64, f64)> = Vec::new();
    let (mut sx, mut sy): (i64, i64) = (0, 0);
    let (mut dx, mut dy): (i64, i64) = (0, -1);
    loop {
        let dec = step * sy as f64 + dec0;
        if dec.abs() <= 90.0 {
            let cosd = dec.to_radians().cos();
            let ra = if cosd.abs() > 1e-6 {
                ra0 + step * sx as f64 / cosd
            } else {
                ra0
            };
            let ra = ra.rem_euclid(360.0);
            // Circular mask: keep cells within the search disk (+½ step so
            // the disk edge is fully covered), reject square-spiral corners.
            if angular_distance(ra, dec, ra0, dec0) <= radius + step * 0.5 {
                out.push((ra, dec));
            }
        }
        // ASTAP exit: checked AFTER emitting the cell.
        if sx > max_distance {
            break;
        }
        // ASTAP turning-point: switch direction at the spiral corners.
        if sx == sy
            || (sx < 0 && sx == -sy)
            || (sx > 0 && sx == 1 - sy)
        {
            let t = dx;
            dx = -dy;
            dy = t;
        }
        sx += dx;
        sy += dy;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiny_radius_yields_just_the_center() {
        let c = spiral_cells(Some((123.0, 45.0)), 0.1, 1.0);
        assert_eq!(c.len(), 1);
        assert!((c[0].0 - 123.0).abs() < 1e-9 && (c[0].1 - 45.0).abs() < 1e-9);
    }

    #[test]
    fn hinted_disk_has_no_coverage_gaps() {
        // Every point within `radius` of center must be within one step of
        // some emitted cell (spiral tiles the tangent lattice at `step`).
        let (ra0, dec0, radius, step) = (200.0, 30.0, 4.0, 1.0);
        let cells = spiral_cells(Some((ra0, dec0)), radius, step);
        assert!(cells.len() > 30);
        let mut worst = 0.0f64;
        let mut t = 0.0;
        while t < radius {
            for k in 0..24 {
                let ang = (k as f64) * std::f64::consts::TAU / 24.0;
                let dec = dec0 + t * ang.sin();
                let cosd = dec.to_radians().cos().max(1e-6);
                let ra = ra0 + t * ang.cos() / cosd;
                let best = cells
                    .iter()
                    .map(|&(cr, cd)| angular_distance(ra, dec, cr, cd))
                    .fold(f64::MAX, f64::min);
                worst = worst.max(best);
            }
            t += 0.25;
        }
        assert!(worst <= step * 1.1, "coverage gap {worst} > {step}");
    }

    #[test]
    fn blind_is_full_sky_and_starts_at_origin() {
        let c = spiral_cells(None, 0.0, 9.5);
        assert_eq!(c[0], (0.0, 0.0));
        // radius 180 / step 9.5 ≈ 19 → a large masked disk of cells.
        assert!(c.len() > 200);
        for &(ra, dec) in &c {
            assert!((0.0..360.0).contains(&ra));
            assert!((-90.0..=90.0).contains(&dec));
        }
    }

    #[test]
    fn ra_step_widens_toward_the_pole() {
        // High-dec cells get a larger RA offset (÷cos dec).
        let cells = spiral_cells(Some((100.0, 80.0)), 6.0, 2.0);
        let max_ra_off = cells
            .iter()
            .map(|&(ra, _)| ((ra - 100.0 + 180.0).rem_euclid(360.0) - 180.0).abs())
            .fold(0.0f64, f64::max);
        // At dec≈80°, cos≈0.17 → RA offsets exceed the 2° step several-fold.
        assert!(max_ra_off > 4.0);
    }
}

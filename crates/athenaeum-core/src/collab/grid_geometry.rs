//! The canonical grid's geometry (spec §6.2, plan 6b-2 C16): its TAN
//! projection, the deterministic proposal from a project's manifest, and
//! the "at least half inside" count. Gated `render+solver`: it reads manifest
//! WCS through `PlateSolveRecord::to_solution()` (SIP applied), the one TAN
//! implementation the stacking run's WCS seed shares.

use solvemyastro::wcs::WcsSolution;

use crate::collab::frame_meta::ProjectFrameMeta;
use crate::collab::grid::{CanonicalGrid, ProposalRotation, RotationSource};

pub struct GridTarget {
    pub ra_deg: f64,
    pub dec_deg: f64,
    pub radius_deg: f64,
}

/// A frame's WCS and pixel extent.
pub struct FrameFootprint {
    pub wcs: WcsSolution,
    pub width: f64,
    pub height: f64,
}

impl FrameFootprint {
    /// The frame's outline on the sky: RA/Dec in degrees of [`boundary_points`] (the pixel edges, eight
    /// points per edge so SIP curvature shows) through the frame's WCS, SIP included — the outline the
    /// proposal measures its box on.
    pub fn outline_sky(&self) -> Vec<[f64; 2]> {
        boundary_points(self.width, self.height)
            .into_iter()
            .map(|(x, y)| {
                let (ra, dec) = self.wcs.pixel_to_sky(x, y);
                [ra, dec]
            })
            .collect()
    }
}

/// One accepted frame as the proposal sees it.
pub struct GridInput {
    /// The frame's manifest uuid and file name, so the rotation can name the frame it follows. Empty from
    /// [`grid_input_of`], which reads the metadata only; a proposal's caller fills them in.
    pub frame_uuid: String,
    pub file_name: String,
    pub pixel_scale_arcsec: Option<f64>,
    /// `meta.snrWeight` when finite and positive.
    pub weight: Option<f64>,
    pub footprint: Option<FrameFootprint>,
}

/// `meta`'s scale and weight, and its footprint when it carries a WCS (SIP
/// included) and its pixel size.
pub fn grid_input_of(meta: &ProjectFrameMeta) -> GridInput {
    let footprint = match (&meta.wcs, meta.naxis1, meta.naxis2) {
        (Some(wcs), Some(w), Some(h)) if w > 0 && h > 0 => wcs
            .to_record(meta.pixel_scale_arcsec)
            .to_solution()
            .map(|wcs| FrameFootprint {
                wcs,
                width: w as f64,
                height: h as f64,
            }),
        _ => None,
    };
    GridInput {
        frame_uuid: String::new(),
        file_name: String::new(),
        pixel_scale_arcsec: meta
            .pixel_scale_arcsec
            .filter(|s| s.is_finite() && *s > 0.0),
        weight: meta.snr_weight.filter(|w| w.is_finite() && *w > 0.0),
        footprint,
    }
}

/// The grid's TAN (sky-standard, C16): `crpix` the 0-based centre
/// `((W-1)/2, (H-1)/2)`, CD in the FITS CROTA2 convention with
/// `CDELT1 = -s` (east left) and `CDELT2 = +s`, so `det < 0` and
/// `field_rotation_deg()` reads `rotationDeg` back.
pub fn grid_wcs(grid: &CanonicalGrid) -> WcsSolution {
    let s = grid.scale_arcsec / 3600.0;
    let (sin, cos) = grid.rotation_deg.to_radians().sin_cos();
    WcsSolution {
        crpix: (
            (grid.width_px as f64 - 1.0) / 2.0,
            (grid.height_px as f64 - 1.0) / 2.0,
        ),
        crval: (grid.crval1, grid.crval2),
        cd: [[-s * cos, -s * sin], [-s * sin, s * cos]],
        sip_forward: None,
        sip_reverse: None,
    }
}

/// A frame's outline: corners plus eight points per edge (SIP and the
/// TAN-to-TAN mapping curve the edges slightly), at the pixel extents, in
/// perimeter order, clockwise in pixel space (y down): the top edge from the
/// top-left corner, then the right, bottom and left edges, each edge starting
/// at its own first corner, so a polygon through the points in list order
/// follows the frame's border.
pub(crate) fn boundary_points(w: f64, h: f64) -> Vec<(f64, f64)> {
    const STEPS: usize = 8;
    let (x0, x1, y0, y1) = (-0.5, w - 0.5, -0.5, h - 0.5);
    let t = |k: usize| k as f64 / STEPS as f64;
    let mut pts = Vec::with_capacity(4 * STEPS);
    for k in 0..STEPS {
        pts.push((x0 + t(k) * (x1 - x0), y0));
    }
    for k in 0..STEPS {
        pts.push((x1, y0 + t(k) * (y1 - y0)));
    }
    for k in 0..STEPS {
        pts.push((x1 - t(k) * (x1 - x0), y1));
    }
    for k in 0..STEPS {
        pts.push((x0, y1 - t(k) * (y1 - y0)));
    }
    pts
}

/// The proposal (spec §6.2): deterministic for a manifest, with sorted
/// inputs and fixed rounding.
/// - Scale: the median `pixelScaleArcsec` (even count: the mean of the two
///   middle ones), rounded to 0.01 arcsec. `None` when no frame has a scale,
///   or the rounded scale is outside the hub's `(0, 100]`.
/// - Centre: the target. Rotation: the long side of one frame, so it lies
///   wide on the grid ([`proposal_rotation`]: the frame pinned as the
///   reference, else the best-weighted one, else the frames' mean axis).
/// - Size: the bounding box, in the proposed (turned) TAN, of the WCS frames'
///   footprints, symmetric about the target and clipped to the target circle's
///   TAN radius, rounded up to a multiple of 64 and clamped to `[64, 65536]`.
///   With no WCS frame, a square of twice the radius.
pub fn propose_grid(
    inputs: &[GridInput],
    target: &GridTarget,
    reference_uuid: Option<&str>,
) -> Option<Proposal> {
    let mut scales: Vec<f64> = inputs.iter().filter_map(|i| i.pixel_scale_arcsec).collect();
    if scales.is_empty() {
        return None;
    }
    scales.sort_by(f64::total_cmp);
    let n = scales.len();
    let median = if n % 2 == 1 {
        scales[n / 2]
    } else {
        0.5 * (scales[n / 2 - 1] + scales[n / 2])
    };
    let scale = (median * 100.0).round() / 100.0;
    if !(scale > 0.0 && scale <= 100.0) {
        return None;
    }
    let r_px = target.radius_deg.to_radians().tan().to_degrees() * 3600.0 / scale;
    // A 1x1 grid puts the reference pixel at (0, 0): grid pixels are offsets
    // from the target.
    let offsets = |rotation_deg: f64| {
        grid_wcs(&CanonicalGrid {
            scale_arcsec: scale,
            width_px: 1,
            height_px: 1,
            crval1: target.ra_deg,
            crval2: target.dec_deg,
            rotation_deg,
        })
    };
    let (rotation, source) = proposal_rotation(inputs, reference_uuid, &offsets(0.0));
    let tan = offsets(rotation);
    let (mut hx, mut hy, mut any) = (0.0f64, 0.0f64, false);
    for fp in inputs.iter().filter_map(|i| i.footprint.as_ref()) {
        for (x, y) in boundary_points(fp.width, fp.height) {
            let (ra, dec) = fp.wcs.pixel_to_sky(x, y);
            let (gx, gy) = tan.sky_to_pixel(ra, dec);
            if gx.is_finite() && gy.is_finite() {
                hx = hx.max(gx.abs());
                hy = hy.max(gy.abs());
                any = true;
            }
        }
    }
    let (hx, hy) = if any {
        (hx.min(r_px), hy.min(r_px))
    } else {
        (r_px, r_px)
    };
    let size = |half: f64| (((2.0 * half) / 64.0).ceil() as i64 * 64).clamp(64, 65536);
    Some(Proposal {
        grid: CanonicalGrid {
            scale_arcsec: scale,
            width_px: size(hx),
            height_px: size(hy),
            crval1: target.ra_deg,
            crval2: target.dec_deg,
            rotation_deg: rotation,
        }
        .normalized(),
        rotation: source,
    })
}

/// A proposal: the grid and where its rotation comes from.
#[derive(Debug, Clone, PartialEq)]
pub struct Proposal {
    pub grid: CanonicalGrid,
    pub rotation: ProposalRotation,
}

/// The proposal's rotation and its source (owner ruling 2026-11-07). The grid turns with the long side of one
/// frame with a footprint: the one `reference_uuid` names (the reference pinned on this device), else the one
/// with the highest weight (ties: the lowest uuid). With no pinned frame and no weight, the frames' mean axis;
/// with no footprint at all, north up.
fn proposal_rotation(
    inputs: &[GridInput],
    reference_uuid: Option<&str>,
    tan0: &WcsSolution,
) -> (f64, ProposalRotation) {
    let with_footprint = || inputs.iter().filter(|i| i.footprint.is_some());
    let following = |input: &GridInput, source: RotationSource| {
        let fp = input.footprint.as_ref().expect("filtered on a footprint");
        (
            frames_rotation(&[fp], tan0),
            ProposalRotation {
                source,
                file_name: Some(input.file_name.clone()),
            },
        )
    };
    if let Some(reference) =
        reference_uuid.and_then(|uuid| with_footprint().find(|i| i.frame_uuid == uuid))
    {
        return following(reference, RotationSource::Reference);
    }
    let best = with_footprint()
        .filter_map(|i| i.weight.map(|w| (w, i)))
        .max_by(|(a, x), (b, y)| a.total_cmp(b).then_with(|| y.frame_uuid.cmp(&x.frame_uuid)));
    if let Some((_, best)) = best {
        return following(best, RotationSource::BestWeighted);
    }
    let footprints: Vec<&FrameFootprint> = with_footprint()
        .filter_map(|i| i.footprint.as_ref())
        .collect();
    let source = if footprints.is_empty() {
        RotationSource::NorthUp
    } else {
        RotationSource::FramesMean
    };
    (
        frames_rotation(&footprints, tan0),
        ProposalRotation {
            source,
            file_name: None,
        },
    )
}

/// The rotation that lays `footprints`' long side horizontal. Each frame gives the axis from its centre along its
/// long side (its x axis when square), mapped through its WCS onto `tan0`, the proposal's TAN at rotation 0. A
/// grid turned by `theta` turns every pixel vector by `+theta`, so an axis at `phi` lies horizontal at
/// `theta = -phi` (mod 180 degrees). Several axes are averaged as doubled angles (axes at 80 and 100 degrees share
/// 90, not 0), summed in sorted order so the result does not depend on frame order. Kept within (-90, 90] (north
/// as near the top as the frames allow) and rounded to 0.01 degree; 0 with no frame, or when the axes cancel.
fn frames_rotation(footprints: &[&FrameFootprint], tan0: &WcsSolution) -> f64 {
    let mut doubled: Vec<f64> = footprints
        .iter()
        .filter_map(|fp| {
            let (cx, cy) = ((fp.width - 1.0) / 2.0, (fp.height - 1.0) / 2.0);
            let (ex, ey) = if fp.width >= fp.height {
                (fp.width - 0.5, cy)
            } else {
                (cx, fp.height - 0.5)
            };
            let on_grid = |x: f64, y: f64| {
                let (ra, dec) = fp.wcs.pixel_to_sky(x, y);
                tan0.sky_to_pixel(ra, dec)
            };
            let ((x0, y0), (x1, y1)) = (on_grid(cx, cy), on_grid(ex, ey));
            let phi = (y1 - y0).atan2(x1 - x0);
            phi.is_finite().then_some(-2.0 * phi)
        })
        .collect();
    doubled.sort_by(f64::total_cmp);
    let (c, s) = doubled
        .iter()
        .fold((0.0f64, 0.0f64), |(c, s), a| (c + a.cos(), s + a.sin()));
    if c.hypot(s) <= 1e-9 * doubled.len() as f64 {
        return 0.0;
    }
    let theta = ((0.5 * s.atan2(c).to_degrees()) * 100.0).round() / 100.0;
    if theta <= -90.0 {
        theta + 180.0
    } else {
        theta
    }
}

/// The fraction of a frame inside the grid: a 16 x 16 lattice of the frame's
/// cell centres mapped onto the grid's pixels (deterministic; no polygon
/// clip needed for a "half inside" verdict).
pub fn fraction_inside(fp: &FrameFootprint, grid: &CanonicalGrid) -> f64 {
    const N: usize = 16;
    let g = grid_wcs(grid);
    let (w, h) = (grid.width_px as f64, grid.height_px as f64);
    let mut inside = 0usize;
    for j in 0..N {
        for i in 0..N {
            let x = (i as f64 + 0.5) * fp.width / N as f64 - 0.5;
            let y = (j as f64 + 0.5) * fp.height / N as f64 - 0.5;
            let (ra, dec) = fp.wcs.pixel_to_sky(x, y);
            // The far hemisphere (separation >= 90 degrees from the grid
            // centre) has no TAN image; the projection would fold it back.
            let cos_sep = dec.to_radians().sin() * grid.crval2.to_radians().sin()
                + dec.to_radians().cos()
                    * grid.crval2.to_radians().cos()
                    * (ra - grid.crval1).to_radians().cos();
            if cos_sep <= 0.0 {
                continue;
            }
            let (gx, gy) = g.sky_to_pixel(ra, dec);
            if gx >= -0.5 && gx < w - 0.5 && gy >= -0.5 && gy < h - 0.5 {
                inside += 1;
            }
        }
    }
    inside as f64 / (N * N) as f64
}

/// How many frames with a footprint lie at least half inside the grid
/// (spec §6.2, §6.3). A frame without a WCS cannot be judged and is not
/// counted.
pub fn count_inside(inputs: &[GridInput], grid: &CanonicalGrid) -> usize {
    inputs
        .iter()
        .filter_map(|i| i.footprint.as_ref())
        .filter(|fp| fraction_inside(fp, grid) >= 0.5)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::frame_meta::{ManifestWcs, ProjectFrameMeta};

    /// The proposal's grid with no reference pinned.
    fn proposed(inputs: &[GridInput], target: &GridTarget) -> Option<CanonicalGrid> {
        propose_grid(inputs, target, None).map(|p| p.grid)
    }

    /// [`grid_input_of`] named and weighted as a proposal's caller fills it in.
    fn named(m: &ProjectFrameMeta, uuid: &str, weight: Option<f64>) -> GridInput {
        GridInput {
            frame_uuid: uuid.to_string(),
            file_name: format!("{uuid}.fits"),
            weight,
            ..grid_input_of(m)
        }
    }

    const TARGET: GridTarget = GridTarget {
        ra_deg: 10.5,
        dec_deg: 41.25,
        radius_deg: 1.0,
    };

    /// A frame centred at `(ra, dec)`, `w × h` pixels at 2″/px, unrotated, in
    /// the frames' own parity (det CD > 0); `crpix` 1-based as the manifest has it.
    fn meta(ra: f64, dec: f64, w: i64, h: i64, scale: Option<f64>) -> ProjectFrameMeta {
        let s = 2.0 / 3600.0;
        ProjectFrameMeta {
            exptime_sec: 300.0,
            naxis1: Some(w),
            naxis2: Some(h),
            pixel_scale_arcsec: scale,
            wcs: Some(ManifestWcs {
                crval1: ra,
                crval2: dec,
                crpix1: (w as f64 + 1.0) / 2.0,
                crpix2: (h as f64 + 1.0) / 2.0,
                cd: [s, 0.0, 0.0, s],
                sip: None,
            }),
            ..ProjectFrameMeta::default()
        }
    }

    /// [`meta`] turned on the sky: the frame's +x axis points `angle` degrees from east towards north, still in
    /// the frames' own parity (det CD > 0).
    fn turned(ra: f64, dec: f64, w: i64, h: i64, angle: f64) -> ProjectFrameMeta {
        let s = 2.0 / 3600.0;
        let (sin, cos) = angle.to_radians().sin_cos();
        let mut m = meta(ra, dec, w, h, Some(2.0));
        m.wcs.as_mut().unwrap().cd = [s * cos, -s * sin, s * sin, s * cos];
        m
    }

    fn scale_only(scale: f64) -> ProjectFrameMeta {
        ProjectFrameMeta {
            exptime_sec: 300.0,
            pixel_scale_arcsec: Some(scale),
            ..ProjectFrameMeta::default()
        }
    }

    #[test]
    fn the_scale_is_the_median_rounded_and_no_scale_means_no_proposal() {
        let inputs: Vec<GridInput> = [1.861, 1.884, 1.9]
            .iter()
            .map(|s| grid_input_of(&scale_only(*s)))
            .collect();
        assert_eq!(proposed(&inputs, &TARGET).unwrap().scale_arcsec, 1.88);
        let even: Vec<GridInput> = [1.0, 2.0]
            .iter()
            .map(|s| grid_input_of(&scale_only(*s)))
            .collect();
        assert_eq!(proposed(&even, &TARGET).unwrap().scale_arcsec, 1.5);
        let none = vec![grid_input_of(&ProjectFrameMeta {
            exptime_sec: 1.0,
            ..ProjectFrameMeta::default()
        })];
        assert_eq!(proposed(&none, &TARGET), None);
    }

    /// No frame with a WCS: a square of twice the target circle's TAN radius,
    /// rounded up to 64 — 1° at 2″/px is 1800.18 px, so 3600.4 → 3648.
    #[test]
    fn without_a_wcs_the_grid_is_the_target_square() {
        let g = proposed(&[grid_input_of(&scale_only(2.0))], &TARGET).unwrap();
        assert_eq!((g.width_px, g.height_px), (3648, 3648));
        assert_eq!((g.crval1, g.crval2, g.rotation_deg), (10.5, 41.25, 0.0));
    }

    /// The footprints' bounding box, symmetric about the target, rounded up
    /// to 64; clipped to the target circle when the circle is smaller.
    #[test]
    fn the_size_is_the_footprint_box_clipped_to_the_target_circle() {
        let inputs = vec![grid_input_of(&meta(10.5, 41.25, 100, 80, Some(2.0)))];
        let g = proposed(&inputs, &TARGET).unwrap();
        assert_eq!((g.width_px, g.height_px), (128, 128));
        let tiny = GridTarget {
            radius_deg: 0.01,
            ..TARGET
        };
        let g = proposed(&inputs, &tiny).unwrap();
        assert_eq!(
            (g.width_px, g.height_px),
            (64, 64),
            "36 px of circle round up to 64"
        );
    }

    /// A frame whose long side runs north-south: the grid turns with it, so the long side lies along the grid's
    /// x axis and the grid is wide, not tall (200 px → 256, 80 px → 128).
    #[test]
    fn the_grid_turns_so_the_frames_long_side_is_horizontal() {
        let inputs = vec![grid_input_of(&turned(10.5, 41.25, 200, 80, 90.0))];
        let g = proposed(&inputs, &TARGET).unwrap();
        assert_eq!(g.rotation_deg, 90.0);
        assert_eq!((g.width_px, g.height_px), (256, 128));
    }

    /// The rotation is the frames' long axis, kept within (-90, 90] so north stays as near the top as the frames
    /// allow, rounded to 0.01 degree. A portrait frame's long side is its y axis.
    #[test]
    fn the_rotation_follows_the_long_axis_nearest_north_up() {
        for (w, h, angle, expected) in [
            (200, 80, 0.0, 0.0),
            (200, 80, 30.0, 30.0),
            (200, 80, 120.0, -60.0),
            (200, 80, -100.0, 80.0),
            (200, 80, 180.0, 0.0),
            (80, 200, 0.0, 90.0),
            (200, 80, 12.3456, 12.35),
        ] {
            let inputs = vec![grid_input_of(&turned(10.5, 41.25, w, h, angle))];
            let g = proposed(&inputs, &TARGET).unwrap();
            assert_eq!(g.rotation_deg, expected, "{w}x{h} at {angle}");
        }
    }

    /// Axes, not directions: frames at 80 and 100 degrees share the 90-degree axis (a plain mean of -80 and 80
    /// would say 0), and frames at 10 and 170 degrees share 0.
    #[test]
    fn the_rotation_is_the_axial_mean_of_the_frames() {
        let pair = |a: f64, b: f64| {
            let inputs = vec![
                grid_input_of(&turned(10.5, 41.25, 200, 80, a)),
                grid_input_of(&turned(10.5, 41.25, 200, 80, b)),
            ];
            proposed(&inputs, &TARGET).unwrap().rotation_deg
        };
        assert_eq!(pair(80.0, 100.0), 90.0);
        assert_eq!(pair(10.0, 170.0), 0.0);
        assert_eq!(pair(20.0, 40.0), 30.0);
    }

    /// Owner ruling 2026-11-07: the grid follows the pinned reference frame's long side, else the best-weighted
    /// frame's, and says which frame it follows.
    #[test]
    fn the_rotation_follows_the_reference_else_the_best_weighted_frame() {
        let inputs = vec![
            named(&turned(10.5, 41.25, 200, 80, 30.0), "a", Some(5.0)),
            named(&turned(10.5, 41.25, 200, 80, 60.0), "b", Some(10.0)),
            named(&scale_only(2.0), "c", Some(99.0)),
        ];
        let pinned = propose_grid(&inputs, &TARGET, Some("a")).unwrap();
        assert_eq!(pinned.grid.rotation_deg, 30.0);
        assert_eq!(
            pinned.rotation,
            ProposalRotation {
                source: RotationSource::Reference,
                file_name: Some("a.fits".into())
            }
        );
        let best = propose_grid(&inputs, &TARGET, None).unwrap();
        assert_eq!(best.grid.rotation_deg, 60.0, "c weighs most but has no WCS");
        assert_eq!(
            best.rotation,
            ProposalRotation {
                source: RotationSource::BestWeighted,
                file_name: Some("b.fits".into())
            }
        );
        let unsolved_pin = propose_grid(&inputs, &TARGET, Some("c")).unwrap();
        assert_eq!(unsolved_pin.rotation.source, RotationSource::BestWeighted);
        let unknown_pin = propose_grid(&inputs, &TARGET, Some("gone")).unwrap();
        assert_eq!(unknown_pin.grid.rotation_deg, 60.0);
    }

    /// Equal weights pick the lowest uuid, so the proposal does not depend on frame order.
    #[test]
    fn a_weight_tie_picks_the_lowest_uuid() {
        let a = named(&turned(10.5, 41.25, 200, 80, 20.0), "a", Some(7.0));
        let b = named(&turned(10.5, 41.25, 200, 80, 40.0), "b", Some(7.0));
        let p = propose_grid(&[b, a], &TARGET, None).unwrap();
        assert_eq!(p.grid.rotation_deg, 20.0);
        assert_eq!(p.rotation.file_name.as_deref(), Some("a.fits"));
    }

    /// No weight anywhere: the frames' mean axis, naming no frame; no WCS frame: north up.
    #[test]
    fn without_weights_the_mean_and_without_a_wcs_north_up() {
        let unweighted = vec![
            named(&turned(10.5, 41.25, 200, 80, 20.0), "a", None),
            named(&turned(10.5, 41.25, 200, 80, 40.0), "b", None),
        ];
        let p = propose_grid(&unweighted, &TARGET, None).unwrap();
        assert_eq!(p.grid.rotation_deg, 30.0);
        assert_eq!(
            p.rotation,
            ProposalRotation {
                source: RotationSource::FramesMean,
                file_name: None
            }
        );
        let p = propose_grid(&[named(&scale_only(2.0), "c", Some(3.0))], &TARGET, None).unwrap();
        assert_eq!(p.grid.rotation_deg, 0.0);
        assert_eq!(p.rotation.source, RotationSource::NorthUp);
    }

    /// `meta.snrWeight` reaches the input only when finite and positive.
    #[test]
    fn the_weight_is_the_manifests_snr_weight() {
        let weighted = |w: Option<f64>| ProjectFrameMeta {
            snr_weight: w,
            ..scale_only(2.0)
        };
        assert_eq!(grid_input_of(&weighted(Some(170.6))).weight, Some(170.6));
        for bad in [Some(0.0), Some(-1.0), Some(f64::NAN), None] {
            assert_eq!(grid_input_of(&weighted(bad)).weight, None, "{bad:?}");
        }
    }

    #[test]
    fn the_proposal_does_not_depend_on_frame_order() {
        let a = meta(10.5, 41.25, 100, 80, Some(2.0));
        let b = meta(10.52, 41.25, 100, 80, Some(2.1));
        let c = scale_only(1.9);
        let d = turned(10.51, 41.25, 100, 80, 33.3);
        let forward = proposed(
            &[
                grid_input_of(&a),
                grid_input_of(&b),
                grid_input_of(&c),
                grid_input_of(&d),
            ],
            &TARGET,
        );
        let reverse = proposed(
            &[
                grid_input_of(&d),
                grid_input_of(&c),
                grid_input_of(&b),
                grid_input_of(&a),
            ],
            &TARGET,
        );
        assert_eq!(forward, reverse);
    }

    #[test]
    fn a_frame_counts_as_inside_when_at_least_half_of_it_is() {
        let grid = CanonicalGrid {
            scale_arcsec: 2.0,
            width_px: 128,
            height_px: 128,
            crval1: 10.5,
            crval2: 41.25,
            rotation_deg: 0.0,
        };
        let inside = grid_input_of(&meta(10.5, 41.25, 100, 80, Some(2.0)));
        assert_eq!(
            fraction_inside(inside.footprint.as_ref().unwrap(), &grid),
            1.0
        );
        let away = grid_input_of(&meta(12.5, 41.25, 100, 80, Some(2.0)));
        assert_eq!(
            fraction_inside(away.footprint.as_ref().unwrap(), &grid),
            0.0
        );
        assert_eq!(
            count_inside(&[inside, away, grid_input_of(&scale_only(2.0))], &grid),
            1
        );
    }

    /// The grid is sky-standard (C16): det CD < 0, the centre pixel maps to
    /// the centre, and the rotation reads back as `field_rotation_deg`.
    #[test]
    fn the_grid_wcs_is_sky_standard_and_reads_back_its_rotation() {
        for rotation in [0.0, 30.0, -30.0] {
            let g = CanonicalGrid {
                scale_arcsec: 2.0,
                width_px: 128,
                height_px: 64,
                crval1: 10.5,
                crval2: 41.25,
                rotation_deg: rotation,
            };
            let wcs = grid_wcs(&g);
            let det = wcs.cd[0][0] * wcs.cd[1][1] - wcs.cd[0][1] * wcs.cd[1][0];
            assert!(det < 0.0);
            let (ra, dec) = wcs.pixel_to_sky(wcs.crpix.0, wcs.crpix.1);
            assert!((ra - 10.5).abs() < 1e-12 && (dec - 41.25).abs() < 1e-12);
            assert!(
                (wcs.field_rotation_deg() - f64::rem_euclid(rotation, 360.0)).abs() < 1e-9,
                "{rotation}"
            );
            assert_eq!(wcs.crpix, (63.5, 31.5));
        }
    }

    #[test]
    fn a_rounded_scale_outside_the_hubs_range_proposes_nothing() {
        let inputs = vec![grid_input_of(&scale_only(0.004))];
        assert_eq!(proposed(&inputs, &TARGET), None);
        let huge = vec![grid_input_of(&scale_only(100.006))];
        assert_eq!(proposed(&huge, &TARGET), None);
    }

    /// A frame shifted north by half its height covers exactly 8 of 16 rows,
    /// so it counts as inside: the comparison is `>= 0.5`, not `> 0.5`.
    #[test]
    fn exactly_half_inside_counts_as_inside() {
        let grid = CanonicalGrid {
            scale_arcsec: 2.0,
            width_px: 64,
            height_px: 64,
            crval1: 10.5,
            crval2: 41.25,
            rotation_deg: 0.0,
        };
        let half = grid_input_of(&meta(10.5, 41.25 + 64.0 / 3600.0, 64, 64, Some(2.0)));
        assert_eq!(
            fraction_inside(half.footprint.as_ref().unwrap(), &grid),
            0.5
        );
        assert_eq!(count_inside(&[half], &grid), 1);
    }

    #[test]
    fn a_frame_at_the_antipode_is_not_inside() {
        let grid = CanonicalGrid {
            scale_arcsec: 2.0,
            width_px: 128,
            height_px: 128,
            crval1: 10.5,
            crval2: 41.25,
            rotation_deg: 0.0,
        };
        let anti = grid_input_of(&meta(190.5, -41.25, 100, 80, Some(2.0)));
        assert_eq!(
            fraction_inside(anti.footprint.as_ref().unwrap(), &grid),
            0.0
        );
        assert_eq!(count_inside(&[anti], &grid), 0);
    }

    /// A footprint's outline is the pixel-edge boundary through its WCS: 32 points, the first at the
    /// top-left pixel corner, and every point inverts back to its pixel edge point.
    #[test]
    fn an_outline_inverts_to_the_pixel_edge_points() {
        let input = grid_input_of(&meta(10.5, 41.25, 100, 80, Some(2.0)));
        let fp = input.footprint.as_ref().unwrap();
        let outline = fp.outline_sky();
        let edge = boundary_points(100.0, 80.0);
        assert_eq!(outline.len(), 32);
        assert_eq!(outline.len(), edge.len());
        assert_eq!(edge[0], (-0.5, -0.5));
        for (sky, (x, y)) in outline.iter().zip(&edge) {
            let (px, py) = fp.wcs.sky_to_pixel(sky[0], sky[1]);
            assert!((px - x).abs() < 1e-6 && (py - y).abs() < 1e-6, "{sky:?}");
        }
    }

    /// The outline is a closed walk along the frame's border (a client draws it as a polygon in list order):
    /// it starts at the top-left corner, runs the top edge first, and every step, the last one back to the
    /// start included, moves along one axis by one eighth of that axis.
    #[test]
    fn boundary_points_run_along_the_perimeter() {
        let (w, h) = (100.0, 80.0);
        let pts = boundary_points(w, h);
        assert_eq!(pts.len(), 32);
        assert_eq!(pts[0], (-0.5, -0.5));
        assert!((pts[1].0 - (-0.5 + w / 8.0)).abs() < 1e-12 && pts[1].1 == -0.5);
        assert!((pts[8].0 - (w - 0.5)).abs() < 1e-12 && pts[8].1 == -0.5);
        for i in 0..pts.len() {
            let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
            let (dx, dy) = ((b.0 - a.0).abs(), (b.1 - a.1).abs());
            let along_x = dy < 1e-12 && (dx - w / 8.0).abs() < 1e-12;
            let along_y = dx < 1e-12 && (dy - h / 8.0).abs() < 1e-12;
            assert!(
                along_x || along_y,
                "step {i} -> {} jumps: {a:?} -> {b:?}",
                (i + 1) % pts.len()
            );
        }
    }

    /// The projection every grid-drawing client repeats (plan 6c Task 7's TS asserts these numbers to
    /// 1e-6 px): four sky points in front of the tangent plane (the grid centre, 0.5 deg east, 0.5 deg
    /// north, one near the south-east pixel corner) projected to 0-based grid pixels with [`grid_wcs`], for a
    /// grid at rotation 0 and one at 30 degrees.
    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct GoldenPoint {
        ra_deg: f64,
        dec_deg: f64,
        x: f64,
        y: f64,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct GoldenGrid {
        grid: CanonicalGrid,
        points: Vec<GoldenPoint>,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Golden {
        grids: Vec<GoldenGrid>,
    }

    fn golden() -> Golden {
        // Nine decimals: far below the 1e-6 px the clients assert, and a text the file keeps short.
        let round = |v: f64| (v * 1e9).round() / 1e9;
        let grids = [0.0, 30.0]
            .into_iter()
            .map(|rotation_deg| {
                let grid = CanonicalGrid {
                    scale_arcsec: 2.0,
                    width_px: 4096,
                    height_px: 3072,
                    crval1: 83.8,
                    crval2: -5.4,
                    rotation_deg,
                };
                let tan = grid_wcs(&grid);
                let cos_dec = grid.crval2.to_radians().cos();
                let points = [
                    (grid.crval1, grid.crval2),
                    (grid.crval1 + 0.5 / cos_dec, grid.crval2),
                    (grid.crval1, grid.crval2 + 0.5),
                    (grid.crval1 + 1.0 / cos_dec, grid.crval2 - 0.7),
                ]
                .into_iter()
                .map(|(ra, dec)| {
                    let (ra_deg, dec_deg) = (round(ra), round(dec));
                    let (x, y) = tan.sky_to_pixel(ra_deg, dec_deg);
                    GoldenPoint {
                        ra_deg,
                        dec_deg,
                        x: round(x),
                        y: round(y),
                    }
                })
                .collect();
                GoldenGrid { grid, points }
            })
            .collect();
        Golden { grids }
    }

    /// The golden file is what the Rust projection computes (the `ts_contract` pattern): regenerate with
    /// `GOLDEN_WRITE=1 cargo test -p athenaeum-core --lib grid_projection_golden`. The check compares
    /// numbers, not text: the last bits of a trig result may differ between platforms.
    #[test]
    fn grid_projection_golden_file_is_current() {
        let computed = golden();
        let centre = &computed.grids[0].points[0];
        assert!(
            (centre.x - 2047.5).abs() < 1e-6 && (centre.y - 1535.5).abs() < 1e-6,
            "the grid centre projects to the centre pixel: {centre:?}"
        );
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../src/components/stacking/gridProjection.golden.json");
        if std::env::var("GOLDEN_WRITE").is_ok() {
            let mut text = serde_json::to_string_pretty(&computed).unwrap();
            text.push('\n');
            std::fs::write(&path, text).unwrap();
            return;
        }
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!("{}: {e}\nRegenerate: GOLDEN_WRITE=1 cargo test -p athenaeum-core --lib grid_projection_golden", path.display())
        });
        let on_disk: Golden = serde_json::from_str(&text).expect("the golden file parses");
        let stale = "stale golden file; regenerate: GOLDEN_WRITE=1 cargo test -p athenaeum-core --lib grid_projection_golden";
        assert_eq!(on_disk.grids.len(), computed.grids.len(), "{stale}");
        for (a, b) in on_disk.grids.iter().zip(&computed.grids) {
            assert_eq!(a.grid, b.grid, "{stale}");
            assert_eq!(a.points.len(), b.points.len(), "{stale}");
            for (p, q) in a.points.iter().zip(&b.points) {
                let close = |u: f64, v: f64| (u - v).abs() < 1e-7;
                assert!(
                    close(p.ra_deg, q.ra_deg)
                        && close(p.dec_deg, q.dec_deg)
                        && close(p.x, q.x)
                        && close(p.y, q.y),
                    "{stale}: {p:?} vs {q:?}"
                );
            }
        }
    }
}

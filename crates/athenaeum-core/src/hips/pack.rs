//! Offline pack selection (issue #18, phase 2): turns "whole sky to order k
//! plus detail around chosen regions" into the deduplicated list of tiles to
//! download. Pure computation, no I/O; the runner lives beside it.
//!
//! The renderer falls back to *ancestors only*, so a detail region covers
//! every order from `k + 1` up to [`super::DISPLAY_MAX_ORDER`]: a gap between
//! k and the deepest order would show the coarse sky.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use super::DISPLAY_MAX_ORDER;
use crate::models::ImagingLocation;
use crate::plate_solve::dso_lookup::DsoCatalog;

/// Deepest whole-sky order a pack may use. Order 6 for the whole sky is
/// ~49 000 tiles and is never a pack.
pub const MAX_SKY_ORDER: u8 = 5;
/// Radius around a catalog object, degrees.
const CATALOG_RADIUS_DEG: f64 = 3.0;
/// Margin added to half the field diagonal of an imaging location, degrees.
const LOCATION_MARGIN_DEG: f64 = 1.0;
/// Radius of an imaging location that has no field of view, degrees.
const LOCATION_NO_FOV_RADIUS_DEG: f64 = 2.0;

/// What the user ticked on the Settings page.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PackSelection {
    pub survey_id: String,
    /// Whole-sky order, 0..=5.
    pub sky_order: u8,
    pub around_locations: bool,
    pub around_messier: bool,
    pub around_ngc_ic: bool,
}

impl Default for PackSelection {
    fn default() -> Self {
        Self {
            survey_id: "dss-color".to_string(),
            sky_order: 4,
            around_locations: true,
            around_messier: true,
            around_ngc_ic: false,
        }
    }
}

/// A cone on the sky that gets full detail.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PackRegion {
    pub ra_deg: f64,
    pub dec_deg: f64,
    pub radius_deg: f64,
}

/// Validate a whole-sky order (0..=5).
pub fn checked_sky_order(order: u8) -> Result<u8> {
    if order > MAX_SKY_ORDER {
        bail!("whole-sky pack order must be 0–5");
    }
    Ok(order)
}

/// One region per imaging location (frame set or cluster): half the diagonal
/// of its field of view plus 1 degree, or 2 degrees when it has no field of
/// view. Locations without a finite position are skipped.
pub fn location_regions(locs: &[ImagingLocation]) -> Vec<PackRegion> {
    locs.iter()
        .filter(|l| l.ra.is_finite() && l.dec.is_finite())
        .map(|l| {
            let radius_deg = match (l.fov_width, l.fov_height) {
                (Some(w), Some(h)) if w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0 => {
                    0.5 * w.hypot(h) + LOCATION_MARGIN_DEG
                }
                _ => LOCATION_NO_FOV_RADIUS_DEG,
            };
            PackRegion {
                ra_deg: l.ra,
                dec_deg: l.dec,
                radius_deg,
            }
        })
        .collect()
}

/// One 3 degree region per catalog object of the ticked groups. Messier means
/// a designation starting with `"M "`; NGC/IC means the first word is `NGC` or
/// `IC` and the magnitude is 10 or brighter.
pub fn catalog_regions(cat: &DsoCatalog, messier: bool, ngc_ic: bool) -> Vec<PackRegion> {
    cat.entries()
        .iter()
        .filter(|d| {
            let first = d.designation.split_whitespace().next().unwrap_or("");
            let is_messier = d.designation.starts_with("M ");
            let is_ngc_ic =
                matches!(first, "NGC" | "IC") && d.magnitude.is_some_and(|m| m <= 10.0);
            (messier && is_messier) || (ngc_ic && is_ngc_ic)
        })
        .map(|d| PackRegion {
            ra_deg: d.ra_deg,
            dec_deg: d.dec_deg,
            radius_deg: CATALOG_RADIUS_DEG,
        })
        .collect()
}

/// Cells of `order` that touch the cone, or `None` when the region is not a
/// usable cone.
///
/// `cdshealpix` asserts on a knife-edge case: a cone whose centre latitude
/// +/- radius straddles the base-cell boundary at +/-41.81 degrees can panic
/// (M 31 is such a field). The radius is only ever nudged *up*, so a retry
/// still covers the whole cone.
fn cone_cells(order: u8, region: &PackRegion) -> Option<Vec<u64>> {
    if !region.ra_deg.is_finite()
        || !region.dec_deg.is_finite()
        || !region.radius_deg.is_finite()
        || region.radius_deg <= 0.0
    {
        return None;
    }
    let lon = region.ra_deg.rem_euclid(360.0).to_radians();
    let lat = region.dec_deg.clamp(-90.0, 90.0).to_radians();
    let radius = region.radius_deg.min(90.0);
    for nudge in [1.0, 1.0001, 1.001, 1.01, 1.05] {
        let radius_rad = (radius * nudge).to_radians();
        let cells = std::panic::catch_unwind(|| {
            cdshealpix::nested::cone_coverage_approx(order, lon, lat, radius_rad).to_flat_array()
        });
        if let Ok(cells) = cells {
            return Some(cells.into_vec());
        }
    }
    None
}

/// The tiles of a pack as `(order, npix)`, sorted ascending and deduplicated:
/// every tile of orders `0..=sky_order`, then for each region the cone
/// coverage at orders `sky_order + 1..=6`.
pub fn pack_tiles(sky_order: u8, regions: &[PackRegion]) -> Result<Vec<(u8, u64)>> {
    let sky_order = checked_sky_order(sky_order)?;
    let mut tiles: Vec<(u8, u64)> = Vec::new();
    for order in 0..=sky_order {
        let n = 12u64 << (2 * u32::from(order));
        tiles.extend((0..n).map(|npix| (order, npix)));
    }
    for region in regions {
        for order in sky_order + 1..=DISPLAY_MAX_ORDER {
            match cone_cells(order, region) {
                Some(cells) => tiles.extend(cells.into_iter().map(|npix| (order, npix))),
                None => tracing::warn!(
                    ra_deg = region.ra_deg,
                    dec_deg = region.dec_deg,
                    radius_deg = region.radius_deg,
                    order,
                    "pack_region_skipped"
                ),
            }
        }
    }
    tiles.sort_unstable();
    tiles.dedup();
    Ok(tiles)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m31() -> PackRegion {
        PackRegion {
            ra_deg: 10.6847,
            dec_deg: 41.2690,
            radius_deg: 3.0,
        }
    }

    fn location(fov: Option<(f64, f64)>) -> ImagingLocation {
        ImagingLocation {
            id: 1,
            ra: 10.0,
            dec: 20.0,
            object_name: None,
            frame_count: 1,
            total_exposure: 1.0,
            filters: vec![],
            date_range: (String::new(), String::new()),
            frame_set_id: None,
            fov_width: fov.map(|f| f.0),
            fov_height: fov.map(|f| f.1),
            location_type: "frameset".to_string(),
            cameras: None,
            focal_lengths: None,
            is_custom: false,
            rotation: None,
        }
    }

    #[test]
    fn whole_sky_counts() {
        assert_eq!(pack_tiles(3, &[]).unwrap().len(), 1020);
        assert_eq!(pack_tiles(4, &[]).unwrap().len(), 4092);
    }

    #[test]
    fn whole_sky_order_6_refused() {
        assert!(checked_sky_order(6).is_err());
        assert!(pack_tiles(6, &[]).is_err());
        assert_eq!(checked_sky_order(5).unwrap(), 5);
        assert_eq!(checked_sky_order(0).unwrap(), 0);
    }

    #[test]
    fn region_adds_orders_above_sky_only() {
        let base = pack_tiles(4, &[]).unwrap();
        let with = pack_tiles(4, &[m31()]).unwrap();
        let added: Vec<_> = with.iter().filter(|t| !base.contains(t)).collect();
        assert!(added.iter().all(|(o, _)| *o == 5 || *o == 6), "{added:?}");
        // The whole-sky part is untouched.
        assert!(base.iter().all(|t| with.contains(t)));
        let o6 = added.iter().filter(|(o, _)| *o == 6).count();
        assert!((30..=90).contains(&o6), "order-6 tiles: {o6}");
        // Every order between k and 6 is present, or the renderer would
        // fall back to the coarse sky.
        assert!(added.iter().any(|(o, _)| *o == 5));
    }

    #[test]
    fn overlapping_regions_dedup() {
        assert_eq!(
            pack_tiles(4, &[m31(), m31()]).unwrap(),
            pack_tiles(4, &[m31()]).unwrap()
        );
    }

    #[test]
    fn no_regions_is_whole_sky_only() {
        assert_eq!(
            pack_tiles(4, &location_regions(&[])).unwrap(),
            pack_tiles(4, &[]).unwrap()
        );
    }

    #[test]
    fn location_radius_from_fov() {
        let r = location_regions(&[location(Some((2.0, 1.5))), location(None)]);
        assert_eq!(r.len(), 2);
        assert!((r[0].radius_deg - 2.25).abs() < 1e-12);
        assert_eq!(r[1].radius_deg, 2.0);
    }

    #[test]
    fn catalog_groups_counts() {
        let cat = DsoCatalog::load().unwrap();
        assert_eq!(catalog_regions(&cat, true, false).len(), 108);
        assert_eq!(catalog_regions(&cat, false, true).len(), 491);
        assert_eq!(catalog_regions(&cat, true, true).len(), 108 + 491);
        assert!(catalog_regions(&cat, false, false).is_empty());
    }

    #[test]
    fn degenerate_region_adds_nothing_and_does_not_panic() {
        let bad = PackRegion {
            ra_deg: f64::NAN,
            dec_deg: 0.0,
            radius_deg: 3.0,
        };
        assert_eq!(pack_tiles(4, &[bad]).unwrap(), pack_tiles(4, &[]).unwrap());
    }

    /// The base-cell boundary at +/-41.81 degrees must not take the pack down.
    #[test]
    fn knife_edge_cones_do_not_panic() {
        for dec in [41.60, 41.71, 41.81, 41.90, -41.71] {
            let r = PackRegion {
                ra_deg: 10.68,
                dec_deg: dec,
                radius_deg: 3.0,
            };
            let t = pack_tiles(4, &[r]).unwrap();
            assert!(t.len() > pack_tiles(4, &[]).unwrap().len(), "dec {dec}");
        }
    }
}

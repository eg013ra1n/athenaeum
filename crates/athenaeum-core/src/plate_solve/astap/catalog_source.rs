//! Pluggable catalog source for per-trial quad construction.
//!
//! ASTAP reads catalog stars for each (FOV, sky-cell) trial from its star
//! database (`read_stars`). We abstract that read behind a trait so the
//! Tycho-2 healpix catalog works now and a deeper catalog (Gaia) can drop
//! in later — needed for true-blind wide/headerless fields — with zero
//! solver changes.

use astroimage::platesolving::CatalogStar;

use crate::catalog::CatalogEngine;

/// A source of catalog stars within a cone, for building catalog quads at
/// a trial's center/scale.
pub trait CatalogQuadSource {
    /// Stars within `radius_deg` of `(ra,dec)` brighter than `mag_limit`,
    /// proper-motion-corrected to `epoch`. Returns empty on read error or an
    /// empty region (the caller then skips the cell — never panics).
    fn stars_in_cone(
        &self,
        ra: f64,
        dec: f64,
        radius_deg: f64,
        mag_limit: f32,
        epoch: f64,
    ) -> Vec<CatalogStar>;
}

/// The default source: the installed Tycho-2 healpix catalog via
/// [`CatalogEngine::cone_search`] (which itself prefers Gaia DR3 if a
/// `gaia_dr3` dir is present — so this is already the Gaia drop-in seam).
pub struct Tycho2ConeSource<'a> {
    catalog: &'a CatalogEngine,
}

impl<'a> Tycho2ConeSource<'a> {
    pub fn new(catalog: &'a CatalogEngine) -> Self {
        Self { catalog }
    }
}

impl CatalogQuadSource for Tycho2ConeSource<'_> {
    fn stars_in_cone(
        &self,
        ra: f64,
        dec: f64,
        radius_deg: f64,
        mag_limit: f32,
        epoch: f64,
    ) -> Vec<CatalogStar> {
        self.catalog
            .cone_search(ra, dec, radius_deg, mag_limit, epoch)
            .map(|(stars, _name)| stars)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::binary_format::StarRecord;
    use crate::catalog::write_catalog_to_healpix;
    use tempfile::TempDir;

    #[test]
    fn tycho2_source_returns_cone_stars() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("tycho2");
        let records = vec![
            StarRecord::from_values(179.5, 44.8, 5.0, 0.0, 0.0),
            StarRecord::from_values(180.0, 45.0, 6.0, 0.0, 0.0),
            StarRecord::from_values(180.5, 45.2, 7.5, 0.0, 0.0),
            StarRecord::from_values(179.8, 44.5, 9.0, 0.0, 0.0),
            StarRecord::from_values(10.0, -30.0, 4.0, 0.0, 0.0),
        ];
        write_catalog_to_healpix(&records, &dir).unwrap();

        let engine = CatalogEngine::with_catalog_dir(tmp.path());
        let src = Tycho2ConeSource::new(&engine);

        let stars = src.stars_in_cone(180.0, 45.0, 2.0, 12.0, 2000.0);
        assert!(stars.len() >= 3, "expected the RA180/Dec45 cluster");
        // The far-away star must not appear in this cone.
        assert!(stars.iter().all(|s| s.dec > 0.0));

        // Empty/elsewhere region → empty, never panics.
        assert!(src.stars_in_cone(60.0, 0.0, 1.0, 12.0, 2000.0).is_empty());
    }
}

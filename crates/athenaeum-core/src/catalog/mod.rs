pub mod binary_format;
pub mod gaia;
pub mod gaia_bulk;
pub mod gaia_prebuilt;
pub mod healpix;
pub mod manifest;
pub mod tycho2;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use memmap2::Mmap;

use astroimage::platesolving::{CatalogStar, ProperMotionCorrector};

use binary_format::{read_records_until_mag, StarRecord};
use healpix::cone_search_pixels;

/// Information about an installed catalog.
#[derive(Clone, Debug)]
pub struct CatalogInfo {
    pub name: String,
    pub path: PathBuf,
    pub epoch: f64,
    pub star_count_approx: u64,
    pub mag_limit: f32,
}

/// Engine for querying HEALpix-indexed star catalogs on disk.
pub struct CatalogEngine {
    tycho2_path: Option<PathBuf>,
    gaia_path: Option<PathBuf>,
}

impl CatalogEngine {
    pub fn new() -> Self {
        CatalogEngine {
            tycho2_path: None,
            gaia_path: None,
        }
    }

    /// Initialize with a catalog directory. Scans for available catalogs.
    pub fn with_catalog_dir(catalog_dir: &Path) -> Self {
        let tycho2_path = catalog_dir.join("tycho2");
        let gaia_path = catalog_dir.join("gaia_dr3");

        CatalogEngine {
            tycho2_path: if tycho2_path.exists() {
                Some(tycho2_path)
            } else {
                None
            },
            gaia_path: if gaia_path.exists() {
                Some(gaia_path)
            } else {
                None
            },
        }
    }

    /// Set the Tycho-2 catalog path explicitly.
    pub fn set_tycho2_path(&mut self, path: PathBuf) {
        self.tycho2_path = Some(path);
    }

    /// Query stars within a cone on the sky, epoch-corrected.
    ///
    /// Automatically selects the best available catalog (Gaia > Tycho-2).
    ///
    /// - `ra_deg`, `dec_deg`: cone center in degrees
    /// - `radius_deg`: search radius in degrees
    /// - `mag_limit`: maximum magnitude to return
    /// - `observation_epoch`: Julian epoch of the observation (e.g., 2025.5)
    pub fn cone_search(
        &self,
        ra_deg: f64,
        dec_deg: f64,
        radius_deg: f64,
        mag_limit: f32,
        observation_epoch: f64,
    ) -> Result<(Vec<CatalogStar>, &str)> {
        // Priority: Gaia DR3 > Tycho-2
        if let Some(ref gaia_path) = self.gaia_path {
            let stars = self.cone_search_catalog(
                gaia_path,
                2016.0, // Gaia DR3 epoch
                ra_deg,
                dec_deg,
                radius_deg,
                mag_limit,
                observation_epoch,
            )?;
            return Ok((stars, "gaia_dr3"));
        }

        if let Some(ref tycho2_path) = self.tycho2_path {
            let stars = self.cone_search_catalog(
                tycho2_path,
                2000.0, // Tycho-2 epoch (J2000)
                ra_deg,
                dec_deg,
                radius_deg,
                mag_limit,
                observation_epoch,
            )?;
            return Ok((stars, "tycho2"));
        }

        Err(anyhow::anyhow!(
            "No star catalog available. Install Tycho-2 or Gaia DR3."
        ))
    }

    fn cone_search_catalog(
        &self,
        catalog_path: &Path,
        catalog_epoch: f64,
        ra_deg: f64,
        dec_deg: f64,
        radius_deg: f64,
        mag_limit: f32,
        observation_epoch: f64,
    ) -> Result<Vec<CatalogStar>> {
        let pixel_ids = cone_search_pixels(ra_deg, dec_deg, radius_deg);
        let mut stars = Vec::new();

        for pid in &pixel_ids {
            let file_path = catalog_path.join(format!("healpix_{:06}.bin", pid));
            if !file_path.exists() {
                continue;
            }

            let file = std::fs::File::open(&file_path)
                .with_context(|| format!("Failed to open catalog file: {}", file_path.display()))?;

            // Safety: the file is read-only and not modified while mapped
            let mmap = unsafe { Mmap::map(&file) }
                .with_context(|| format!("Failed to mmap catalog file: {}", file_path.display()))?;

            let records = read_records_until_mag(&mmap, mag_limit);

            for record in &records {
                let (ra_corr, dec_corr) = ProperMotionCorrector::propagate(
                    record.ra as f64,
                    record.dec as f64,
                    record.pmra_mas_yr(),
                    record.pmdec_mas_yr(),
                    catalog_epoch,
                    observation_epoch,
                );

                stars.push(CatalogStar {
                    ra: ra_corr,
                    dec: dec_corr,
                    mag: record.mag(),
                });
            }
        }

        Ok(stars)
    }

    /// List available catalogs.
    pub fn available_catalogs(&self) -> Vec<CatalogInfo> {
        let mut catalogs = Vec::new();

        if let Some(ref path) = self.tycho2_path {
            catalogs.push(CatalogInfo {
                name: "Tycho-2".to_string(),
                path: path.clone(),
                epoch: 2000.0,
                star_count_approx: 2_500_000,
                mag_limit: 11.5,
            });
        }

        if let Some(ref path) = self.gaia_path {
            catalogs.push(CatalogInfo {
                name: "Gaia DR3".to_string(),
                path: path.clone(),
                epoch: 2016.0,
                star_count_approx: 100_000_000,
                mag_limit: 14.0,
            });
        }

        catalogs
    }

    /// Check if any catalog is available.
    pub fn has_catalog(&self) -> bool {
        self.tycho2_path.is_some() || self.gaia_path.is_some()
    }

    /// Load catalog stars in a single HEALpix region (no neighbors).
    /// The region size should match the image FOV for blind solving.
    ///
    /// `healpix_pixel`: pixel index at `query_depth`
    /// `query_depth`: HEALpix depth for the region
    pub fn load_region(
        &self,
        healpix_pixel: u64,
        query_depth: u8,
        mag_limit: f32,
        observation_epoch: f64,
    ) -> Result<(Vec<CatalogStar>, &str)> {
        let catalog_path;
        let catalog_epoch;
        let catalog_name;

        if let Some(ref gaia_path) = self.gaia_path {
            catalog_path = gaia_path;
            catalog_epoch = 2016.0;
            catalog_name = "gaia_dr3";
        } else if let Some(ref tycho2_path) = self.tycho2_path {
            catalog_path = tycho2_path;
            catalog_epoch = 2000.0;
            catalog_name = "tycho2";
        } else {
            return Err(anyhow::anyhow!("No star catalog available"));
        }

        // Only load the central pixel's sub-pixels, not neighbors.
        // This keeps the star set concentrated in the region matching the image FOV.
        let depth6_pixels = healpix::sub_pixels_at_depth6(healpix_pixel, query_depth);
        let mut stars = Vec::new();

        for pid in &depth6_pixels {
            let file_path = catalog_path.join(format!("healpix_{:06}.bin", pid));
            if !file_path.exists() {
                continue;
            }

            let file = std::fs::File::open(&file_path)?;
            let mmap = unsafe { Mmap::map(&file) }?;
            let records = binary_format::read_records_until_mag(&mmap, mag_limit);

            for record in &records {
                let (ra_corr, dec_corr) = ProperMotionCorrector::propagate(
                    record.ra as f64,
                    record.dec as f64,
                    record.pmra_mas_yr(),
                    record.pmdec_mas_yr(),
                    catalog_epoch,
                    observation_epoch,
                );

                stars.push(CatalogStar {
                    ra: ra_corr,
                    dec: dec_corr,
                    mag: record.mag(),
                });
            }
        }

        Ok((stars, catalog_name))
    }

    /// Total number of HEALpix regions at a given depth.
    pub fn region_count(depth: u8) -> u64 {
        healpix::n_pixels_at_depth(depth)
    }

    /// Get the center (RA, Dec) in degrees of a HEALpix pixel at a given depth.
    pub fn region_center(pixel: u64, depth: u8) -> (f64, f64) {
        healpix::pixel_center(depth, pixel)
    }
}

/// Write a set of star records to HEALpix-indexed binary files.
///
/// Each star is assigned to a HEALpix pixel based on its RA/Dec.
/// Files are written to `output_dir/healpix_NNNNNN.bin`.
pub fn write_catalog_to_healpix(
    records: &[StarRecord],
    output_dir: &Path,
) -> Result<()> {
    use std::collections::HashMap;
    use std::io::BufWriter;

    std::fs::create_dir_all(output_dir)?;

    // Bin records by HEALpix pixel
    let mut bins: HashMap<u64, Vec<StarRecord>> = HashMap::new();
    for record in records {
        let pixel = healpix::sky_to_pixel(record.ra as f64, record.dec as f64);
        bins.entry(pixel).or_default().push(record.clone());
    }

    // Write each bin to a file
    for (pixel, mut stars) in bins {
        let file_path = output_dir.join(format!("healpix_{:06}.bin", pixel));
        let file = std::fs::File::create(&file_path)?;
        let mut writer = BufWriter::new(file);
        binary_format::write_records(&mut writer, &mut stars)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_test_catalog(dir: &Path) {
        let records: Vec<StarRecord> = vec![
            // Stars around RA=180, Dec=45
            StarRecord::from_values(179.5, 44.8, 5.0, 0.0, 0.0),
            StarRecord::from_values(180.0, 45.0, 6.0, 10.0, -5.0),
            StarRecord::from_values(180.5, 45.2, 7.5, -3.0, 8.0),
            StarRecord::from_values(179.8, 44.5, 9.0, 0.0, 0.0),
            StarRecord::from_values(180.2, 45.5, 10.0, 0.0, 0.0),
            // Stars in a different region (RA=10, Dec=-30)
            StarRecord::from_values(10.0, -30.0, 4.0, 0.0, 0.0),
            StarRecord::from_values(10.5, -30.2, 8.0, 0.0, 0.0),
        ];

        write_catalog_to_healpix(&records, dir).unwrap();
    }

    #[test]
    fn write_and_read_catalog() {
        let tmp = TempDir::new().unwrap();
        let catalog_dir = tmp.path().join("tycho2");
        make_test_catalog(&catalog_dir);

        let engine = CatalogEngine::with_catalog_dir(tmp.path());
        assert!(engine.has_catalog());

        let (stars, catalog_name) = engine
            .cone_search(180.0, 45.0, 2.0, 12.0, 2000.0)
            .unwrap();

        assert_eq!(catalog_name, "tycho2");
        assert!(
            stars.len() >= 3,
            "Expected >=3 stars near RA=180 Dec=45, got {}",
            stars.len()
        );
    }

    #[test]
    fn mag_limit_filtering() {
        let tmp = TempDir::new().unwrap();
        let catalog_dir = tmp.path().join("tycho2");
        make_test_catalog(&catalog_dir);

        let engine = CatalogEngine::with_catalog_dir(tmp.path());

        let (stars_bright, _) = engine.cone_search(180.0, 45.0, 2.0, 7.0, 2000.0).unwrap();
        let (stars_all, _) = engine.cone_search(180.0, 45.0, 2.0, 12.0, 2000.0).unwrap();

        assert!(
            stars_bright.len() < stars_all.len(),
            "Brighter mag limit should return fewer stars"
        );
    }

    #[test]
    fn proper_motion_changes_positions() {
        let tmp = TempDir::new().unwrap();
        let catalog_dir = tmp.path().join("tycho2");

        let records = vec![
            StarRecord::from_values(180.0, 45.0, 5.0, 500.0, -300.0), // large PM
        ];
        write_catalog_to_healpix(&records, &catalog_dir).unwrap();

        let engine = CatalogEngine::with_catalog_dir(tmp.path());

        let (stars_2000, _) = engine.cone_search(180.0, 45.0, 2.0, 12.0, 2000.0).unwrap();
        let (stars_2025, _) = engine.cone_search(180.0, 45.0, 2.0, 12.0, 2025.0).unwrap();

        assert_eq!(stars_2000.len(), 1);
        assert_eq!(stars_2025.len(), 1);

        let ra_diff = (stars_2025[0].ra - stars_2000[0].ra).abs();
        let dec_diff = (stars_2025[0].dec - stars_2000[0].dec).abs();
        assert!(
            ra_diff > 1e-6 || dec_diff > 1e-6,
            "Proper motion should change positions over 25 years"
        );
    }

    #[test]
    fn no_catalog_returns_error() {
        let tmp = TempDir::new().unwrap();
        let engine = CatalogEngine::with_catalog_dir(tmp.path());
        assert!(!engine.has_catalog());

        let result = engine.cone_search(180.0, 45.0, 2.0, 12.0, 2000.0);
        assert!(result.is_err());
    }
}

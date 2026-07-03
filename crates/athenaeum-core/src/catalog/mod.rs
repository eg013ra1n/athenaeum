pub mod binary_format;
pub mod gaia;
pub mod gaia_bulk;
pub mod gaia_prebuilt;
pub mod healpix;
pub mod manifest;

use std::path::Path;

use anyhow::Result;

use binary_format::StarRecord;

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

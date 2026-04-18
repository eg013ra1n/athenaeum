use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use flate2::read::GzDecoder;

use super::binary_format::StarRecord;
use super::write_catalog_to_healpix;

const TYCHO2_BASE_URL: &str = "https://cdsarc.cds.unistra.fr/ftp/I/259";
const TYCHO2_FILE_COUNT: usize = 20;

/// Progress callback for download and conversion phases.
pub enum Tycho2Progress {
    Downloading { file_index: usize, total_files: usize, bytes: u64 },
    Converting { stars_processed: usize, total_stars: usize },
    Complete { total_stars: usize },
    Error(String),
}

/// Download all 20 Tycho-2 gzipped data files from CDS.
pub fn download_tycho2(
    output_dir: &Path,
    cancel_flag: Arc<AtomicBool>,
    progress: &dyn Fn(Tycho2Progress),
) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(output_dir)?;

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .context("Failed to create HTTP client")?;

    let mut downloaded = Vec::new();

    for i in 0..TYCHO2_FILE_COUNT {
        if cancel_flag.load(Ordering::Relaxed) {
            return Err(anyhow::anyhow!("Download cancelled"));
        }

        let filename = format!("tyc2.dat.{:02}.gz", i);
        let url = format!("{}/{}", TYCHO2_BASE_URL, filename);
        let output_path = output_dir.join(&filename);

        if output_path.exists() {
            eprintln!("tycho2: {} already exists, skipping", filename);
            downloaded.push(output_path);
            progress(Tycho2Progress::Downloading {
                file_index: i + 1,
                total_files: TYCHO2_FILE_COUNT,
                bytes: 0,
            });
            continue;
        }

        eprintln!("tycho2: downloading {}", url);
        let response = client
            .get(&url)
            .send()
            .with_context(|| format!("Failed to download {url}"))?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "HTTP {} downloading {}",
                response.status(),
                url
            ));
        }

        let bytes = response
            .bytes()
            .with_context(|| format!("Failed to read response body for {url}"))?;

        let mut file = std::fs::File::create(&output_path)?;
        file.write_all(&bytes)?;
        downloaded.push(output_path);

        progress(Tycho2Progress::Downloading {
            file_index: i + 1,
            total_files: TYCHO2_FILE_COUNT,
            bytes: bytes.len() as u64,
        });
    }

    Ok(downloaded)
}

/// Parse a single Tycho-2 catalog line (pipe-delimited, 206 bytes).
///
/// Key fields by byte position (0-indexed):
/// - RAmdeg: bytes 15-26 (F12.8, degrees)
/// - DEmdeg: bytes 28-39 (F12.8, degrees)
/// - pmRA:   bytes 41-47 (F7.1, mas/yr, includes cos(dec))
/// - pmDE:   bytes 49-55 (F7.1, mas/yr)
/// - VTmag:  bytes 123-128 (F6.3)
pub fn parse_tycho2_line(line: &str) -> Option<StarRecord> {
    if line.len() < 130 {
        return None;
    }

    let ra_str = line.get(15..27)?.trim();
    let dec_str = line.get(28..40)?.trim();
    let pmra_str = line.get(41..48)?.trim();
    let pmde_str = line.get(49..56)?.trim();
    let vtmag_str = line.get(123..129)?.trim();

    let ra: f64 = ra_str.parse().ok()?;
    let dec: f64 = dec_str.parse().ok()?;
    let vtmag: f32 = vtmag_str.parse().ok()?;

    // Proper motions may be blank for some stars
    let pmra: f64 = pmra_str.parse().unwrap_or(0.0);
    let pmde: f64 = pmde_str.parse().unwrap_or(0.0);

    // Validate ranges
    if ra < 0.0 || ra >= 360.0 || dec < -90.0 || dec > 90.0 {
        return None;
    }
    if vtmag < 0.0 || vtmag > 20.0 {
        return None;
    }

    Some(StarRecord::from_values(
        ra as f32,
        dec as f32,
        vtmag,
        pmra,
        pmde,
    ))
}

/// Convert downloaded Tycho-2 gz files to HEALpix binary catalog format.
pub fn convert_tycho2_to_healpix(
    gz_dir: &Path,
    catalog_dir: &Path,
    cancel_flag: Arc<AtomicBool>,
    progress: &dyn Fn(Tycho2Progress),
) -> Result<usize> {
    let mut all_records = Vec::with_capacity(2_600_000);
    let mut files_found = 0;

    for i in 0..TYCHO2_FILE_COUNT {
        if cancel_flag.load(Ordering::Relaxed) {
            return Err(anyhow::anyhow!("Conversion cancelled"));
        }

        let gz_path = gz_dir.join(format!("tyc2.dat.{:02}.gz", i));
        if !gz_path.exists() {
            eprintln!("tycho2: missing {}, skipping", gz_path.display());
            continue;
        }
        files_found += 1;

        let file = std::fs::File::open(&gz_path)?;
        let decoder = GzDecoder::new(file);
        let reader = BufReader::new(decoder);

        for line in reader.lines() {
            let line = line?;
            if let Some(record) = parse_tycho2_line(&line) {
                all_records.push(record);
            }
        }

        progress(Tycho2Progress::Converting {
            stars_processed: all_records.len(),
            total_stars: 2_539_913,
        });
    }

    if files_found == 0 {
        return Err(anyhow::anyhow!(
            "No Tycho-2 gz files found in {}",
            gz_dir.display()
        ));
    }

    eprintln!(
        "tycho2: parsed {} stars from {} files, writing HEALpix catalog...",
        all_records.len(),
        files_found
    );

    write_catalog_to_healpix(&all_records, catalog_dir)?;

    let total = all_records.len();
    progress(Tycho2Progress::Complete { total_stars: total });

    Ok(total)
}

/// Full pipeline: download Tycho-2 and convert to HEALpix binary.
pub fn setup_tycho2_catalog(
    app_data_dir: &Path,
    cancel_flag: Arc<AtomicBool>,
    progress: &dyn Fn(Tycho2Progress),
) -> Result<PathBuf> {
    let raw_dir = app_data_dir.join("tycho2_raw");
    let catalog_dir = app_data_dir.join("catalogs").join("tycho2");

    if catalog_dir.exists() {
        let file_count = std::fs::read_dir(&catalog_dir)?.count();
        if file_count > 100 {
            eprintln!("tycho2: catalog already exists with {file_count} files");
            return Ok(catalog_dir);
        }
    }

    download_tycho2(&raw_dir, cancel_flag.clone(), progress)?;
    convert_tycho2_to_healpix(&raw_dir, &catalog_dir, cancel_flag, progress)?;

    Ok(catalog_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_line() {
        // Synthetic Tycho-2 line with correct field positions
        // Positions: RA at 15-26, Dec at 28-39, pmRA at 41-47, pmDE at 49-55, VTmag at 123-128
        let mut line = vec![b' '; 206];

        // TYC identifier (cols 0-12)
        let tyc = b"0001 00001 1";
        line[..12].copy_from_slice(tyc);

        // RA (cols 15-26): 180.12345678
        let ra = b"180.12345678";
        line[15..27].copy_from_slice(ra);

        // Dec (cols 28-39): -45.67890123
        let dec = b"-45.67890123";
        line[28..40].copy_from_slice(dec);

        // pmRA (cols 41-47): -12.3
        let pmra = b"  -12.3";
        line[41..48].copy_from_slice(pmra);

        // pmDE (cols 49-55): 45.6
        let pmde = b"   45.6";
        line[49..56].copy_from_slice(pmde);

        // VTmag (cols 123-128): 8.456
        let mag = b" 8.456";
        line[123..129].copy_from_slice(mag);

        let line_str = String::from_utf8(line).unwrap();
        let record = parse_tycho2_line(&line_str).expect("Should parse valid line");

        assert!((record.ra - 180.123).abs() < 0.01, "RA: {}", record.ra);
        assert!((record.dec - (-45.679)).abs() < 0.01, "Dec: {}", record.dec);
        assert!((record.mag() - 8.456).abs() < 0.01, "Mag: {}", record.mag());
        assert!((record.pmra_mas_yr() - (-12.3)).abs() < 0.1);
        assert!((record.pmdec_mas_yr() - 45.6).abs() < 0.1);
    }

    #[test]
    fn parse_blank_proper_motion() {
        let mut line = vec![b' '; 206];
        let ra = b"100.00000000";
        line[15..27].copy_from_slice(ra);
        let dec = b" 30.00000000";
        line[28..40].copy_from_slice(dec);
        // Leave PM fields blank
        let mag = b" 9.000";
        line[123..129].copy_from_slice(mag);

        let line_str = String::from_utf8(line).unwrap();
        let record = parse_tycho2_line(&line_str).expect("Should parse with blank PM");
        assert!((record.pmra_mas_yr()).abs() < 0.01);
        assert!((record.pmdec_mas_yr()).abs() < 0.01);
    }

    #[test]
    fn parse_short_line_returns_none() {
        assert!(parse_tycho2_line("too short").is_none());
    }
}

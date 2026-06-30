//! Build the ready-to-upload publish/ tree: per-tier zips + sha256 + manifest.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

use athenaeum_core::catalog::manifest::{Manifest, ManifestTier};

fn min_fov_for(density: u32) -> f64 {
    match density {
        d if d <= 500 => 0.6,
        d if d <= 2000 => 0.3,
        d if d <= 5000 => 0.2,
        _ => 0.15,
    }
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut h = Sha256::new();
    let mut f = BufReader::new(File::open(path)?);
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}

/// Zip `out_dir/tier_<d>/stars.smac` as `tier_<d>/stars.smac` into the archive
/// (Stored — dense binary; `large_file` for >4 GB tiers).
fn zip_tier(out_dir: &Path, density: u32, zip_path: &Path) -> Result<()> {
    let smac = out_dir.join(format!("tier_{density}")).join("stars.smac");
    let zf = BufWriter::new(File::create(zip_path)?);
    let mut zw = ZipWriter::new(zf);
    let opts = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .large_file(true);
    zw.start_file(format!("tier_{density}/stars.smac"), opts)?;
    let mut f = BufReader::new(
        File::open(&smac).with_context(|| format!("open {}", smac.display()))?,
    );
    io::copy(&mut f, &mut zw)?;
    zw.finish()?;
    Ok(())
}

/// Write `out_dir/publish/{manifest.json, tier_<d>.zip, tier_<d>.zip.sha256}`
/// for every tier; returns the `publish/` path.
pub fn package_publish(out_dir: &Path, tiers: &[(u32, usize)], epoch: f64) -> Result<PathBuf> {
    let pub_dir = out_dir.join("publish");
    fs::create_dir_all(&pub_dir)?;

    let mut manifest_tiers = Vec::new();
    for (density, _count) in tiers {
        let zip_name = format!("tier_{density}.zip");
        let sha_name = format!("{zip_name}.sha256");
        let zip_path = pub_dir.join(&zip_name);

        zip_tier(out_dir, *density, &zip_path)?;
        let digest = sha256_file(&zip_path)?;
        fs::write(pub_dir.join(&sha_name), format!("{digest}  {zip_name}\n"))?;

        manifest_tiers.push(ManifestTier {
            density: *density,
            zip: zip_name,
            sha256: sha_name,
            dir: format!("tier_{density}"),
            size_bytes: fs::metadata(&zip_path)?.len(),
            min_fov_deg: min_fov_for(*density),
        });
        println!("  packaged tier_{density}");
    }

    let manifest = Manifest { version: 1, catalog_epoch: epoch, tiers: manifest_tiers };
    let json = serde_json::to_vec_pretty(&manifest)?;
    fs::write(pub_dir.join("manifest.json"), json)?;
    Ok(pub_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use solvemyastro::cache::build_cache;
    use solvemyastro::StarRecord;

    #[test]
    fn produces_zip_sha_and_manifest_per_tier() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path();
        // build a tiny real tier cache so the zip has a valid stars.smac
        let recs = vec![StarRecord {
            ra: 10.0,
            dec: 20.0,
            mag: 8.0,
            pmra_mas_yr: 0.0,
            pmdec_mas_yr: 0.0,
        }];
        build_cache(recs, &out.join("tier_500"), 2016.0, |_| {}).unwrap();

        let pub_dir = package_publish(out, &[(500, 1)], 2016.0).unwrap();
        assert!(pub_dir.join("tier_500.zip").is_file());
        assert!(pub_dir.join("tier_500.zip.sha256").is_file());

        let m: serde_json::Value =
            serde_json::from_slice(&std::fs::read(pub_dir.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(m["tiers"][0]["density"], 500);
        assert_eq!(m["tiers"][0]["zip"], "tier_500.zip");
        assert_eq!(m["tiers"][0]["dir"], "tier_500");
        assert_eq!(m["tiers"][0]["min_fov_deg"], 0.6);

        // sha256 sidecar matches the zip
        let sidecar = std::fs::read_to_string(pub_dir.join("tier_500.zip.sha256")).unwrap();
        let digest = sidecar.split_whitespace().next().unwrap();
        assert_eq!(digest.len(), 64);
    }
}

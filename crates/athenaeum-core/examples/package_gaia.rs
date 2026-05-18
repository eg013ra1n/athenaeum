//! Package the locally-built Gaia DR3 (G≤16) catalog into a single
//! distributable archive + checksum for hosting on our own server.
//!
//! Run AFTER `ingest_gaia` (or the in-app build) has finished:
//!   cargo run -p athenaeum-core --example package_gaia --release
//!   cargo run -p athenaeum-core --example package_gaia --release -- <app_data_dir>
//!
//! Produces `<app_data>/gaia_dr3_g16.zip` + `.sha256`. Upload BOTH to the
//! server so they are reachable at `GAIA_PREBUILT_URL`
//! (`https://artfrom.space/catalogs/gaia_dr3_g16.zip` + `.sha256`); the
//! in-app "Download Gaia DR3 (prebuilt)" button then fetches + verifies +
//! extracts it. Redistribution of this derived subset is permitted with
//! ESA/Gaia/DPAC credit.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::time::Instant;

use sha2::{Digest, Sha256};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

fn main() -> anyhow::Result<()> {
    let app_data_dir = match std::env::args().nth(1) {
        Some(p) => PathBuf::from(p),
        None => {
            let home = std::env::var("HOME")?;
            PathBuf::from(home).join("Library/Application Support/com.vsharifov.athenaeum")
        }
    };
    let catalog_dir = app_data_dir.join("catalogs").join("gaia_dr3");
    let out_zip = app_data_dir.join("gaia_dr3_g16.zip");
    let out_sha = app_data_dir.join("gaia_dr3_g16.zip.sha256");

    let mut files: Vec<PathBuf> = std::fs::read_dir(&catalog_dir)
        .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", catalog_dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("healpix_") && n.ends_with(".bin"))
                .unwrap_or(false)
        })
        .collect();
    files.sort();
    if files.is_empty() {
        anyhow::bail!(
            "no healpix_*.bin in {} — run the Gaia ingest first",
            catalog_dir.display()
        );
    }
    println!("packaging {} catalog files → {}", files.len(), out_zip.display());

    let start = Instant::now();
    {
        let zf = BufWriter::new(File::create(&out_zip)?);
        let mut zw = ZipWriter::new(zf);
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        let mut buf = vec![0u8; 1024 * 1024];
        for (i, path) in files.iter().enumerate() {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            zw.start_file(name, opts)?;
            let mut f = BufReader::new(File::open(path)?);
            loop {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                zw.write_all(&buf[..n])?;
            }
            if (i + 1) % 4096 == 0 {
                println!("  zipped {}/{}", i + 1, files.len());
            }
        }
        zw.finish()?;
    }

    // Checksum (must match what gaia_prebuilt verifies).
    let mut hasher = Sha256::new();
    let mut f = BufReader::new(File::open(&out_zip)?);
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let sha = format!("{:x}", hasher.finalize());
    std::fs::write(&out_sha, format!("{sha}  gaia_dr3_g16.zip\n"))?;

    let size_mb = std::fs::metadata(&out_zip)?.len() as f64 / 1_048_576.0;
    println!(
        "\ndone in {:.0}s\n  archive : {} ({size_mb:.0} MB)\n  sha256  : {}\n  digest  : {sha}",
        start.elapsed().as_secs_f64(),
        out_zip.display(),
        out_sha.display()
    );
    println!(
        "\nUpload BOTH to the server so they resolve at:\n  \
         https://artfrom.space/catalogs/gaia_dr3_g16.zip\n  \
         https://artfrom.space/catalogs/gaia_dr3_g16.zip.sha256\n\
         e.g.  scp '{}' '{}' <host>:/var/www/artfrom.space/catalogs/",
        out_zip.display(),
        out_sha.display()
    );
    Ok(())
}

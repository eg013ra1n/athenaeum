//! `catalog-builder publish-index`: build the quad-index packs from the
//! density-tier caches and package them into the upload tree.
//!
//! Writes `<out>/publish/{index_base.zip, index_base.zip.sha256,
//! index_narrow.zip, index_narrow.zip.sha256, index_manifest.json}` next to the
//! tier files, each zip holding `<pack>/quads.smaq`. With `--packs-dir` the
//! build is skipped and already-built `<dir>/base/quads.smaq` (and
//! `<dir>/narrow/quads.smaq`) are packaged as they are.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use solvemyastro::index::{build_index, BuildParams, FORMAT_VERSION, INDEX_FILE};
use solvemyastro::{QuadIndex, StarCache};

use athenaeum_core::catalog::manifest::{IndexManifest, IndexManifestPack};

use crate::publish::{write_sha256_sidecar, zip_file};

/// Gaia DR3 reference epoch, used when no tier cache is at hand to read it from.
const DEFAULT_EPOCH: f64 = 2016.0;

/// The packs, in publish order.
const PACKS: [&str; 2] = ["base", "narrow"];

struct Args {
    tiers: Option<PathBuf>,
    out: PathBuf,
    packs_dir: Option<PathBuf>,
    epoch: f64,
}

fn parse(args: impl Iterator<Item = String>) -> Result<Option<Args>> {
    let mut args = args;
    let (mut tiers, mut out, mut packs_dir, mut epoch) = (None, None, None, DEFAULT_EPOCH);
    let val = |args: &mut dyn Iterator<Item = String>, flag: &str| {
        args.next().ok_or_else(|| anyhow!("missing value for {flag}"))
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--tiers" => tiers = Some(PathBuf::from(val(&mut args, "--tiers")?)),
            "--out" => out = Some(PathBuf::from(val(&mut args, "--out")?)),
            "--packs-dir" => packs_dir = Some(PathBuf::from(val(&mut args, "--packs-dir")?)),
            "--epoch" => epoch = val(&mut args, "--epoch")?.parse().context("--epoch")?,
            "-h" | "--help" => {
                print_help();
                return Ok(None);
            }
            other => bail!("unknown argument: {other} (try publish-index --help)"),
        }
    }
    let out = out.ok_or_else(|| anyhow!("--out <dir> is required (try publish-index --help)"))?;
    if tiers.is_none() && packs_dir.is_none() {
        bail!("--tiers <dir> is required unless --packs-dir is given (try publish-index --help)");
    }
    Ok(Some(Args { tiers, out, packs_dir, epoch }))
}

pub fn print_help() {
    println!(
        "catalog-builder publish-index — build & package the quad-index packs\n\
\n\
USAGE:\n  \
  catalog-builder publish-index --tiers <dir> --out <dir>\n  \
  catalog-builder publish-index --packs-dir <dir> --out <dir> [--tiers <dir> | --epoch <year>]\n\
\n\
  --tiers <dir>      Directory holding tier_500/ (and tier_2000/ for the narrow pack).\n  \
  --out <dir>        Output dir: packs are built into <out>/index/, the upload tree into <out>/publish/.\n  \
  --packs-dir <dir>  Skip the build; package the already-built <dir>/base/quads.smaq and\n  \
                     <dir>/narrow/quads.smaq (whichever exist).\n  \
  --epoch <year>     Catalog epoch when no --tiers is given (default {DEFAULT_EPOCH}).\n"
    );
}

/// Entry point for `catalog-builder publish-index <args>`.
pub fn run(args: impl Iterator<Item = String>) -> Result<()> {
    let Some(a) = parse(args)? else { return Ok(()) };
    fs::create_dir_all(&a.out).with_context(|| format!("create --out {}", a.out.display()))?;
    let (epoch, packs_dir) = match &a.packs_dir {
        Some(dir) => {
            println!("[1/2] package already-built packs from {}", dir.display());
            let epoch = match &a.tiers {
                Some(t) => tier_epoch(t)?,
                None => a.epoch,
            };
            (epoch, dir.clone())
        }
        None => {
            let tiers = a.tiers.as_ref().expect("checked by parse");
            let dir = a.out.join("index");
            println!("[1/2] build packs → {}", dir.display());
            let epoch = build_packs(tiers, &dir)?;
            (epoch, dir)
        }
    };
    let pub_dir = package_index_publish(&packs_dir, &a.out, epoch)?;
    println!("[2/2] publish tree → {}", pub_dir.display());
    println!("  upload its contents to artfrom.space/catalogs/ :");
    println!(
        "  scp -P 40022 {}/index_* <user>@artfrom.space:/var/www/artfrom.space/catalogs/",
        pub_dir.display()
    );
    println!("\nDone.");
    Ok(())
}

fn tier_epoch(tiers: &Path) -> Result<f64> {
    let cache = StarCache::open(&tiers.join("tier_500"))
        .with_context(|| format!("open {}/tier_500", tiers.display()))?;
    Ok(cache.catalog_epoch())
}

/// Build one pack from `layers` into `out_dir/quads.smaq`.
fn build_pack(layers: &[&StarCache], out_dir: &Path, params: &BuildParams, name: &str) -> Result<()> {
    let start = Instant::now();
    let (stars, quads) = build_index(layers, out_dir, params, |m| println!("    {m}"))
        .with_context(|| format!("build the {name} pack"))?;
    println!(
        "  {name}: {quads} quads, {stars} stars in {:.0} s",
        start.elapsed().as_secs_f64()
    );
    Ok(())
}

/// Build `base` (depths 2-7, 10 stars, 8 quads per cell) and `narrow` (depth
/// 8 only, 10 stars, 4 quads per cell) under `packs_dir`; returns the catalog
/// epoch of the tier cache.
pub fn build_packs(tiers: &Path, packs_dir: &Path) -> Result<f64> {
    let t500 = StarCache::open(&tiers.join("tier_500"))
        .with_context(|| format!("open {}/tier_500", tiers.display()))?;
    let t2000 = StarCache::open(&tiers.join("tier_2000"))
        .with_context(|| format!("open {}/tier_2000 (the narrow pack needs it)", tiers.display()))?;
    build_pack(&[&t500], &packs_dir.join("base"), &BuildParams::default(), "base")?;
    let narrow = BuildParams { min_depth: 8, max_depth: 8, stars_per_cell: 10, quads_per_cell: 4 };
    build_pack(&[&t500, &t2000], &packs_dir.join("narrow"), &narrow, "narrow")?;
    Ok(t500.catalog_epoch())
}

/// Package every built `<packs_dir>/<pack>/quads.smaq` (`base`, `narrow`) into
/// `<out_dir>/publish/` and write `index_manifest.json`; returns that dir.
pub fn package_index_publish(packs_dir: &Path, out_dir: &Path, epoch: f64) -> Result<PathBuf> {
    let pub_dir = out_dir.join("publish");
    fs::create_dir_all(&pub_dir)?;
    let mut packs = Vec::new();
    for name in PACKS {
        let dir = packs_dir.join(name);
        let file = dir.join(INDEX_FILE);
        if !file.is_file() {
            println!("  no {name} pack at {}, skipped", file.display());
            continue;
        }
        let ix = QuadIndex::open(&dir)
            .with_context(|| format!("the {name} pack at {} does not open", dir.display()))?;
        let min_fov_deg = ix.min_fov_deg();
        drop(ix);

        let zip_name = format!("index_{name}.zip");
        let zip_path = pub_dir.join(&zip_name);
        zip_file(&file, &format!("{name}/{INDEX_FILE}"), &zip_path)?;
        let sha256 = write_sha256_sidecar(&pub_dir, &zip_name, &zip_path)?;
        packs.push(IndexManifestPack {
            name: name.to_string(),
            zip: zip_name,
            sha256,
            dir: name.to_string(),
            size_bytes: fs::metadata(&zip_path)?.len(),
            min_fov_deg,
        });
        println!("  packaged {name}");
    }
    if packs.is_empty() {
        bail!("no pack found under {} (expected base/{INDEX_FILE} or narrow/{INDEX_FILE})", packs_dir.display());
    }
    let manifest = IndexManifest { version: 1, format: FORMAT_VERSION, catalog_epoch: epoch, packs };
    fs::write(pub_dir.join("index_manifest.json"), serde_json::to_vec_pretty(&manifest)?)?;
    Ok(pub_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use solvemyastro::cache::build_cache;
    use solvemyastro::StarRecord;

    /// A tiny real tier cache (a synthetic full sky) in `dir`.
    fn tiny_tier(dir: &Path) {
        let mut s = 11u64;
        let mut rnd = || {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (s >> 11) as f64 / (1u64 << 53) as f64
        };
        let stars: Vec<StarRecord> = (0..8_000)
            .map(|_| StarRecord {
                ra: rnd() * 360.0,
                dec: (rnd() * 2.0 - 1.0).asin().to_degrees(),
                mag: (4.0 + rnd() * 10.0) as f32,
                pmra_mas_yr: 0.0,
                pmdec_mas_yr: 0.0,
            })
            .collect();
        build_cache(stars, dir, 2016.0, |_| {}).unwrap();
    }

    #[test]
    fn packages_built_packs_into_zips_sha_sidecars_and_a_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let tiers = tmp.path().join("tiers");
        tiny_tier(&tiers.join("tier_500"));
        let cache = StarCache::open(&tiers.join("tier_500")).unwrap();
        let packs = tmp.path().join("packs");
        let params = BuildParams { min_depth: 2, max_depth: 2, stars_per_cell: 10, quads_per_cell: 8 };
        build_pack(&[&cache], &packs.join("base"), &params, "base").unwrap();
        build_pack(&[&cache], &packs.join("narrow"), &params, "narrow").unwrap();

        let out = tmp.path().join("out");
        let pub_dir = package_index_publish(&packs, &out, tier_epoch(&tiers).unwrap()).unwrap();
        for name in ["base", "narrow"] {
            assert!(pub_dir.join(format!("index_{name}.zip")).is_file());
            let sidecar =
                fs::read_to_string(pub_dir.join(format!("index_{name}.zip.sha256"))).unwrap();
            assert!(sidecar.ends_with(&format!("  index_{name}.zip\n")), "{sidecar:?}");
            assert_eq!(sidecar.split_whitespace().next().unwrap().len(), 64);
        }
        let m: serde_json::Value =
            serde_json::from_slice(&fs::read(pub_dir.join("index_manifest.json")).unwrap()).unwrap();
        assert_eq!(m["version"], 1);
        assert_eq!(m["format"], FORMAT_VERSION);
        assert_eq!(m["catalog_epoch"], 2016.0);
        assert_eq!(m["packs"][1]["name"], "narrow");
        assert_eq!(m["packs"][0]["zip"], "index_base.zip");
        assert_eq!(m["packs"][0]["sha256"], "index_base.zip.sha256");
        assert_eq!(m["packs"][0]["dir"], "base");
        let want_fov = QuadIndex::open(&packs.join("base")).unwrap().min_fov_deg();
        assert_eq!(m["packs"][0]["min_fov_deg"].as_f64().unwrap(), want_fov);
        let zip_len = fs::metadata(pub_dir.join("index_base.zip")).unwrap().len();
        assert_eq!(m["packs"][0]["size_bytes"].as_u64().unwrap(), zip_len);

        // The zip holds exactly base/quads.smaq, byte for byte.
        let mut z = zip::ZipArchive::new(fs::File::open(pub_dir.join("index_base.zip")).unwrap()).unwrap();
        assert_eq!(z.len(), 1);
        let mut entry = z.by_index(0).unwrap();
        assert_eq!(entry.name(), "base/quads.smaq");
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut bytes).unwrap();
        assert_eq!(bytes, fs::read(packs.join("base").join(INDEX_FILE)).unwrap());
    }

    #[test]
    fn a_missing_pack_is_skipped_and_none_at_all_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let tiers = tmp.path().join("tiers");
        tiny_tier(&tiers.join("tier_500"));
        let cache = StarCache::open(&tiers.join("tier_500")).unwrap();
        let packs = tmp.path().join("packs");
        let params = BuildParams { min_depth: 2, max_depth: 2, stars_per_cell: 10, quads_per_cell: 8 };
        build_pack(&[&cache], &packs.join("base"), &params, "base").unwrap();
        let pub_dir = package_index_publish(&packs, &tmp.path().join("out"), 2016.0).unwrap();
        let m: serde_json::Value =
            serde_json::from_slice(&fs::read(pub_dir.join("index_manifest.json")).unwrap()).unwrap();
        assert_eq!(m["packs"].as_array().unwrap().len(), 1);
        assert!(!pub_dir.join("index_narrow.zip").exists());
        assert!(package_index_publish(&tmp.path().join("none"), &tmp.path().join("out2"), 2016.0).is_err());
    }

    #[test]
    fn the_arguments_need_out_and_tiers_or_packs_dir() {
        let a = |v: &[&str]| parse(v.iter().map(|s| s.to_string()));
        assert!(a(&["--out", "o"]).is_err());
        assert!(a(&["--tiers", "t"]).is_err());
        let ok = a(&["--tiers", "t", "--out", "o"]).unwrap().unwrap();
        assert_eq!((ok.tiers.unwrap(), ok.out), (PathBuf::from("t"), PathBuf::from("o")));
        let ok = a(&["--packs-dir", "p", "--out", "o", "--epoch", "2016.5"]).unwrap().unwrap();
        assert_eq!((ok.packs_dir.unwrap(), ok.epoch), (PathBuf::from("p"), 2016.5));
        assert!(a(&["--nope"]).is_err());
    }
}

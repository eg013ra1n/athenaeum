//! Package writer: copy payload files into a destination directory, write the
//! NDJSON manifest, and produce the [`PackageAnnounce`] that advertises it.

use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use uuid::Uuid;
use xxhash_rust::xxh3::Xxh3;

use crate::sharing::types::{PackageAnnounce, PackageId};

use super::manifest::ManifestRecord;
use super::MANIFEST_FILENAME;

/// Write a package into `dest_dir`: copy each source file to its `rel_path`,
/// emit `manifest.ndjson` (one compact record per line), and return the
/// [`PackageAnnounce`] describing the bundle.
///
/// The caller supplies fully-formed records (including `byte_size` and the
/// full-content `xxh3`); this function writes them verbatim so
/// [`super::validate_package`] can later re-verify the copies against them.
///
/// `rel_path`s must be relative and free of `..` / root / prefix components — a
/// package must never let a record escape its own directory.
pub fn write_package(
    dest_dir: &Path,
    records: Vec<(PathBuf, ManifestRecord)>,
) -> Result<PackageAnnounce> {
    fs::create_dir_all(dest_dir)
        .with_context(|| format!("create package dir {}", dest_dir.display()))?;

    let mut manifest_records: Vec<ManifestRecord> = Vec::with_capacity(records.len());
    let mut total_bytes: u64 = 0;

    for (src, record) in records {
        let rel = Path::new(&record.rel_path);
        if rel
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
        {
            bail!(
                "rel_path must be relative with no '..'/root components: {}",
                record.rel_path
            );
        }

        let dest = dest_dir.join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create payload dir {}", parent.display()))?;
        }
        fs::copy(&src, &dest)
            .with_context(|| format!("copy {} -> {}", src.display(), dest.display()))?;

        total_bytes = total_bytes.saturating_add(record.byte_size);
        manifest_records.push(record);
    }

    // NDJSON: one compact JSON object per line, no pretty-printing.
    let manifest_path = dest_dir.join(MANIFEST_FILENAME);
    let mut buf = String::new();
    for r in &manifest_records {
        let line = serde_json::to_string(r).context("serialize manifest record")?;
        buf.push_str(&line);
        buf.push('\n');
    }
    fs::write(&manifest_path, buf.as_bytes())
        .with_context(|| format!("write manifest {}", manifest_path.display()))?;

    let announce = PackageAnnounce {
        package_id: PackageId(Uuid::new_v4().to_string()),
        root_hash: compute_root_hash(&manifest_records),
        byte_size: total_bytes,
        frame_count: manifest_records.len() as u32,
    };

    tracing::debug!(
        path = %dest_dir.display(),
        count = announce.frame_count,
        bytes = announce.byte_size,
        "package written"
    );

    Ok(announce)
}

/// Placeholder root hash: xxh3 over the payload content-hashes in sorted order.
///
/// Sorting makes it order-independent — reordering members can't change the
/// package's identity. Task A5 replaces this with the iroh collection hash; the
/// field stays an opaque, producer-defined string either way.
fn compute_root_hash(records: &[ManifestRecord]) -> String {
    let mut hashes: Vec<&str> = records.iter().map(|r| r.xxh3.as_str()).collect();
    hashes.sort_unstable();
    let mut hasher = Xxh3::new();
    for h in hashes {
        hasher.update(h.as_bytes());
        hasher.update(b"\n");
    }
    format!("{:016x}", hasher.digest())
}

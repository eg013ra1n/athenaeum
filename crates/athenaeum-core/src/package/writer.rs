//! Package writer: copy payload files into a destination directory, write the
//! NDJSON manifest, and produce the [`PackageAnnounce`] that advertises it.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use uuid::Uuid;
use xxhash_rust::xxh3::Xxh3;

use crate::sharing::types::{PackageAnnounce, PackageId};

use super::manifest::ManifestRecord;
use super::{validate_rel_path, MANIFEST_FILENAME};

/// A hook that computes the package's `root_hash` from the fully-written package
/// directory (payload files + `manifest.ndjson`).
///
/// The default writer path uses an xxh3 placeholder (see [`compute_root_hash`]).
/// Task A5's iroh transport substitutes the iroh-blobs **collection hash** behind
/// this same opaque string field: the caller imports the package directory into
/// its blob store (an async operation), obtains the collection hash, and supplies
/// a closure that returns it. The provider is intentionally synchronous — a
/// caller with an already-computed hash returns it directly; the engine's live
/// send path does not use this hook at all (it overrides `root_hash` with the
/// collection hash at announce time, see `sharing::iroh`).
pub type RootHashProvider<'a> = dyn Fn(&Path) -> Result<String> + 'a;

/// Write a package into `dest_dir`: copy each source file to its `rel_path`,
/// emit `manifest.ndjson` (one compact record per line), and return the
/// [`PackageAnnounce`] describing the bundle. Uses the built-in xxh3 placeholder
/// for `root_hash`; call [`write_package_with_root_hash`] to substitute a
/// different digest (e.g. the iroh collection hash).
pub fn write_package(
    dest_dir: &Path,
    records: Vec<(PathBuf, ManifestRecord)>,
) -> Result<PackageAnnounce> {
    write_package_with_root_hash(dest_dir, records, None)
}

/// Like [`write_package`], but lets the caller override how `root_hash` is
/// computed via an optional [`RootHashProvider`] hook (`None` reproduces the
/// built-in xxh3 placeholder exactly). The hook runs after the package directory
/// — including `manifest.ndjson` — is fully written, receiving the package dir.
///
/// The caller supplies fully-formed records (including `byte_size` and the
/// full-content `xxh3`); this function writes them verbatim so
/// [`super::validate_package`] can later re-verify the copies against them.
///
/// `rel_path`s must be relative and free of `..` / root / prefix components — a
/// package must never let a record escape its own directory.
pub fn write_package_with_root_hash(
    dest_dir: &Path,
    records: Vec<(PathBuf, ManifestRecord)>,
    root_hash: Option<&RootHashProvider<'_>>,
) -> Result<PackageAnnounce> {
    fs::create_dir_all(dest_dir)
        .with_context(|| format!("create package dir {}", dest_dir.display()))?;

    let mut manifest_records: Vec<ManifestRecord> = Vec::with_capacity(records.len());
    let mut total_bytes: u64 = 0;

    for (src, record) in records {
        validate_rel_path(&record.rel_path)?;
        let rel = Path::new(&record.rel_path);

        let dest = dest_dir.join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create payload dir {}", parent.display()))?;
        }
        let copied = fs::copy(&src, &dest)
            .with_context(|| format!("copy {} -> {}", src.display(), dest.display()))?;
        // Integrity guard: the copied file must match the manifest's declared
        // size, else the package advertises a `byte_size`/`xxh3` that its own
        // payload no longer satisfies (truncated read, racing writer, wrong
        // record). Fail loudly here rather than ship a package that fails
        // validation on the receiver.
        if copied != record.byte_size {
            anyhow::bail!(
                "package copy size mismatch for {}: copied {} bytes, manifest byte_size {}",
                record.rel_path,
                copied,
                record.byte_size
            );
        }

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

    let root_hash = match root_hash {
        Some(provider) => provider(dest_dir).context("root-hash provider failed")?,
        None => compute_root_hash(&manifest_records),
    };

    let announce = PackageAnnounce {
        package_id: PackageId(Uuid::new_v4().to_string()),
        root_hash,
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
/// package's identity. Task A5 replaces this with the iroh collection hash (via
/// [`RootHashProvider`] or the transport's announce-time override); the field
/// stays an opaque, producer-defined string either way.
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

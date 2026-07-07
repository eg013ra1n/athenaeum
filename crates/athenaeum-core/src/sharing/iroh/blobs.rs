//! iroh-blobs glue: turn a package directory into a fetchable *collection*, and
//! turn a downloaded collection back into a package directory.
//!
//! A [`package`](crate::package) on disk is `manifest.ndjson` plus payload files
//! addressed by forward-slash `rel_path`. We map that onto an iroh-blobs
//! [`Collection`]: every file (manifest included) is imported as a blob, and the
//! collection is the named sequence `(rel_path → blob hash)`. The collection's
//! own hash is the package's `root_hash` — content-addressed, so an identical
//! package always yields the same hash, and iroh-blobs verifies every byte on
//! download (BLAKE3 bao trees), which is what makes a killed transfer resumable.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use iroh::{Endpoint, EndpointId};
use iroh_blobs::api::downloader::Shuffled;
use iroh_blobs::api::{Store, TempTag};
use iroh_blobs::format::collection::Collection;
use iroh_blobs::{Hash, HashAndFormat};

use crate::package::validate_rel_path;

/// A payload file discovered under a package directory.
struct PkgFile {
    /// Absolute path on disk.
    abs: PathBuf,
    /// Forward-slash path relative to the package root — the collection entry name.
    name: String,
}

/// Recursively list the regular files under a package dir, sorted by name so the
/// collection layout is deterministic (identical package ⇒ identical hash).
fn collect_files(root: &Path) -> Result<Vec<PkgFile>> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(root).sort_by_file_name() {
        let entry = entry.with_context(|| format!("walk {}", root.display()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let abs = entry.path().to_path_buf();
        let rel = abs
            .strip_prefix(root)
            .with_context(|| format!("strip prefix {}", root.display()))?;
        // Collection names use forward slashes regardless of host separator.
        let name = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        out.push(PkgFile { abs, name });
    }
    if out.is_empty() {
        anyhow::bail!("package dir has no files: {}", root.display());
    }
    Ok(out)
}

/// Import every file under `pkg_dir` into `store` and assemble them into a
/// collection. Returns the collection [`Hash`] (the package `root_hash`) after
/// pinning it with a permanent tag so it survives garbage collection and is
/// serveable to peers.
pub async fn import_package_collection(store: &Store, pkg_dir: &Path) -> Result<Hash> {
    let files = collect_files(pkg_dir)?;

    // Hold each child's temp tag alive until the collection is stored + tagged,
    // so nothing it references can be collected mid-assembly.
    let mut child_tags: Vec<TempTag> = Vec::with_capacity(files.len());
    let mut items: Vec<(String, Hash)> = Vec::with_capacity(files.len());
    for f in &files {
        let tt = store
            .blobs()
            .add_path(&f.abs)
            .temp_tag()
            .await
            .with_context(|| format!("import blob {}", f.abs.display()))?;
        items.push((f.name.clone(), tt.hash()));
        child_tags.push(tt);
    }

    let collection = Collection::from_iter(items);
    let tag = collection
        .store(store)
        .await
        .context("store package collection")?;
    // A permanent tag over the hash-seq keeps the whole collection (meta + every
    // child blob) reachable across GC once the temp tags drop.
    store
        .tags()
        .create(tag.hash_and_format())
        .await
        .context("tag package collection")?;
    let hash = tag.hash();
    drop(child_tags);

    tracing::debug!(
        path = %pkg_dir.display(),
        count = files.len(),
        root_hash = %hash,
        "package imported as collection"
    );
    Ok(hash)
}

/// Download the collection identified by `root_hash` from `provider` into
/// `store`, then export every entry to its `rel_path` under `dest_dir`,
/// reconstructing the package directory.
///
/// The download is resumable: iroh-blobs persists verified byte ranges in the
/// store, so a re-invocation after an interrupted transfer only fetches what is
/// missing. `provider` is resolved to a dialable address via the endpoint's
/// address lookup (populated at pairing time).
///
/// Collection entry names are peer-supplied and therefore untrusted, exactly
/// like a manifest record's `rel_path` on the write side — every name is
/// validated with [`crate::package::validate_rel_path`] *before* touching
/// `dest_dir` (not even created) so a malicious entry (`../x`, an absolute
/// path) can neither escape `dest_dir` nor overwrite an arbitrary path; the
/// whole fetch errors instead.
pub async fn fetch_collection_to_dir(
    store: &Store,
    endpoint: &Endpoint,
    provider: EndpointId,
    root_hash: Hash,
    dest_dir: &Path,
) -> Result<()> {
    // Pull the whole hash-sequence (collection meta + every child blob).
    store
        .downloader(endpoint)
        .download(HashAndFormat::hash_seq(root_hash), Shuffled::new(vec![provider]))
        .await
        .with_context(|| format!("download collection {root_hash}"))?;

    // Reconstruct the package directory from the downloaded collection.
    let collection = Collection::load(root_hash, store)
        .await
        .with_context(|| format!("load collection {root_hash}"))?;

    // Validate every entry name before writing anything at all — `dest_dir`
    // isn't even created yet, so a rejected entry leaves no trace on disk.
    for (name, _) in collection.iter() {
        validate_rel_path(name)
            .with_context(|| format!("collection entry name failed validation: {name}"))?;
    }

    tokio::fs::create_dir_all(dest_dir)
        .await
        .with_context(|| format!("create dest dir {}", dest_dir.display()))?;

    for (name, blob_hash) in collection.iter() {
        let target = dest_dir.join(name);
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("create dir {}", parent.display()))?;
        }
        store
            .blobs()
            .export(*blob_hash, &target)
            .await
            .with_context(|| format!("export {name} -> {}", target.display()))?;
    }

    tracing::debug!(
        provider = %provider.fmt_short(),
        root_hash = %root_hash,
        count = collection.len(),
        path = %dest_dir.display(),
        "collection fetched to package dir"
    );
    Ok(())
}

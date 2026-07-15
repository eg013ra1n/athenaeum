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

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use iroh::{Endpoint, EndpointId};
use iroh_blobs::api::downloader::{DownloadProgressItem, Shuffled};
use iroh_blobs::api::{Store, TempTag};
use iroh_blobs::format::collection::Collection;
use iroh_blobs::protocol::{ChunkRanges, GetRequest};
use iroh_blobs::{Hash, HashAndFormat};
use n0_future::StreamExt as _;

use crate::package::{read_manifest, validate_rel_path, ManifestRecord, MANIFEST_FILENAME};
use crate::sharing::types::FetchEvent;
use crate::sharing::FetchSink;

/// Minimum wall-clock gap between two throttled [`FetchEvent`]s from the same
/// source — applied per-file (each observer) AND to the aggregate batch stream.
/// A file's completion event and the final batch event are ALWAYS emitted,
/// throttle or not, so the sink never misses a terminal value. Progress is UI
/// event data, never a log.
const FETCH_PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(300);

/// Aborts every spawned per-file observer task on drop — the ONE place that
/// covers all exit paths from [`fetch_collection_to_dir`]: an early `?`
/// (download-error), the whole `fetch` future being dropped mid-flight
/// (cancellation — the receiver-cancel flow builds on exactly this), or a panic.
/// Without this, a leaked observer keeps its `FetchSink` + `Blobs` clones alive,
/// blocks on `stream.next()` forever for a blob that never completes, and can
/// deliver a stale `File` event after `fetch` already returned. Aborting an
/// already-finished handle is a harmless no-op, so draining the handles on the
/// happy path before the guard drops is safe.
struct AbortObserversOnDrop(Vec<tokio::task::JoinHandle<()>>);

impl Drop for AbortObserversOnDrop {
    fn drop(&mut self) {
        for h in &self.0 {
            h.abort();
        }
    }
}

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
/// pinning it under the deterministic `tag` name so it survives garbage
/// collection and is serveable to peers — and so `release` can later delete it
/// by that exact name.
pub async fn import_package_collection(store: &Store, pkg_dir: &Path, tag: &str) -> Result<Hash> {
    let files = collect_files(pkg_dir)?;
    let count = files.len();

    // Hold each child's temp tag alive until the collection is stored + tagged,
    // so nothing it references can be collected mid-assembly.
    let mut child_tags: Vec<TempTag> = Vec::with_capacity(count);
    let mut items: Vec<(String, Hash)> = Vec::with_capacity(count);
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

    let hash = store_and_tag_collection(store, items, child_tags, tag).await?;
    tracing::debug!(
        path = %pkg_dir.display(),
        count,
        root_hash = %hash,
        "package imported as collection"
    );
    Ok(hash)
}

/// Assemble `items` (entry name → child blob hash) into a [`Collection`], store
/// it, and pin it under the permanent `tag`. `child_tags` holds every child
/// blob's temp tag alive until the collection AND its permanent tag exist —
/// nothing the collection references can be GC'd mid-assembly — then drops.
/// `tags().set` overwrites a same-name tag, so re-serving a package id re-points
/// its tag rather than leaking a second one. Returns the collection [`Hash`]
/// (the package `root_hash`). Shared by the full and want-subset import paths.
async fn store_and_tag_collection(
    store: &Store,
    items: Vec<(String, Hash)>,
    child_tags: Vec<TempTag>,
    tag: &str,
) -> Result<Hash> {
    let collection = Collection::from_iter(items);
    let tag_tt = collection
        .store(store)
        .await
        .context("store package collection")?;
    store
        .tags()
        .set(tag, tag_tt.hash_and_format())
        .await
        .context("tag package collection")?;
    let hash = tag_tt.hash();
    drop(child_tags);
    Ok(hash)
}

/// Import ONLY the negotiated want frames under `pkg_dir` into `store` and
/// assemble them into a collection — the dedup-aware counterpart of
/// [`import_package_collection`], used when the pre-Announce handshake settled
/// on a subset the peer still wants.
///
/// The collection carries a `manifest.ndjson` filtered to exactly the wanted
/// records plus each wanted payload file, so the receiver rebuilds a package of
/// precisely the negotiated frames and never sees the ones it already had. The
/// full package directory on disk is untouched (Task 7 build-once): this reads
/// it and imports only the wanted subset. Entries are sorted by name so an
/// identical want set over an identical package always yields the same
/// `root_hash`, matching `import_package_collection`'s sorted-walk determinism.
///
/// `want` must be non-empty — an all-duplicate package is dropped before serve,
/// never served empty; an empty (or manifest-matching-nothing) want is a caller
/// error and returns `Err`.
pub async fn import_subset_collection(
    store: &Store,
    pkg_dir: &Path,
    want: &HashSet<String>,
    tag: &str,
) -> Result<Hash> {
    if want.is_empty() {
        anyhow::bail!(
            "import_subset_collection called with an empty want set (pkg {})",
            pkg_dir.display()
        );
    }

    // Keep only the manifest records the peer still wants, in manifest order.
    let kept: Vec<ManifestRecord> = read_manifest(pkg_dir)?
        .into_iter()
        .filter(|r| want.contains(&r.rel_path))
        .collect();
    if kept.is_empty() {
        anyhow::bail!(
            "want set matched no manifest records (pkg {})",
            pkg_dir.display()
        );
    }

    // Re-serialize the kept records as the SAME ndjson `read_manifest` parses.
    let mut manifest_ndjson = String::new();
    for r in &kept {
        let line = serde_json::to_string(r).context("serialize filtered manifest record")?;
        manifest_ndjson.push_str(&line);
        manifest_ndjson.push('\n');
    }

    // Collection entries: the filtered manifest (an in-memory blob — no temp
    // file) plus each wanted payload under `pkg_dir`, sorted by entry name.
    enum Src {
        /// Filtered `manifest.ndjson`, added straight from memory.
        ManifestBytes(Vec<u8>),
        /// A wanted payload file at its absolute on-disk path.
        Payload(PathBuf),
    }
    let mut entries: Vec<(String, Src)> = Vec::with_capacity(kept.len() + 1);
    entries.push((
        MANIFEST_FILENAME.to_string(),
        Src::ManifestBytes(manifest_ndjson.into_bytes()),
    ));
    for r in &kept {
        entries.push((r.rel_path.clone(), Src::Payload(pkg_dir.join(&r.rel_path))));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut child_tags: Vec<TempTag> = Vec::with_capacity(entries.len());
    let mut items: Vec<(String, Hash)> = Vec::with_capacity(entries.len());
    for (name, src) in entries {
        let tt = match src {
            Src::ManifestBytes(bytes) => store
                .blobs()
                .add_bytes(bytes)
                .temp_tag()
                .await
                .context("import filtered manifest blob")?,
            Src::Payload(abs) => store
                .blobs()
                .add_path(&abs)
                .temp_tag()
                .await
                .with_context(|| format!("import blob {}", abs.display()))?,
        };
        items.push((name, tt.hash()));
        child_tags.push(tt);
    }

    let count = items.len();
    let hash = store_and_tag_collection(store, items, child_tags, tag).await?;
    tracing::debug!(
        path = %pkg_dir.display(),
        count,
        want = kept.len(),
        root_hash = %hash,
        "package subset imported as collection"
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
    tag: &str,
    dest_dir: &Path,
    byte_size: u64,
    sink: FetchSink,
) -> Result<()> {
    // ONE downloader reused across phase 1 + phase 2: each `store.downloader`
    // spins up a fresh actor + connection pool, so reusing it avoids re-dialing
    // the provider between the two requests.
    let downloader = store.downloader(endpoint);

    // Phase 1: fetch only the hash-seq (root) + collection meta (child 0) so we
    // learn the entry names and per-child hashes before the bulk transfer.
    let meta_req = GetRequest::builder()
        .root(ChunkRanges::all())
        .child(0, ChunkRanges::all())
        .build(root_hash);
    downloader
        .download(meta_req, Shuffled::new(vec![provider]))
        .await
        .with_context(|| format!("download collection meta {root_hash}"))?;

    let collection = Collection::load(root_hash, store)
        .await
        .with_context(|| format!("load collection {root_hash}"))?;

    // Validate every entry name before writing anything at all — `dest_dir`
    // isn't even created yet, so a rejected entry leaves no trace on disk.
    for (name, _) in collection.iter() {
        validate_rel_path(name)
            .with_context(|| format!("collection entry name failed validation: {name}"))?;
    }

    // Per-file observers: one task per child hash. Each streams the child's
    // bitfield and emits a throttled (>=300 ms) `File` progress event, ALWAYS
    // emitting the terminal (is_complete) event so the sink sees each file finish.
    // Held in `AbortObserversOnDrop` from spawn time so every exit path below —
    // early `?`, the whole future being dropped/cancelled, or a panic — aborts
    // any still-running observer instead of leaking it.
    let mut observers = AbortObserversOnDrop(Vec::with_capacity(collection.len()));
    for (name, hash) in collection.iter() {
        let sink = sink.clone();
        let name = name.clone();
        let hash = *hash;
        let blobs = store.blobs().clone();
        observers.0.push(tokio::spawn(async move {
            // Seed `last` in the past so the first update emits immediately.
            let mut last = Instant::now()
                .checked_sub(FETCH_PROGRESS_MIN_INTERVAL)
                .unwrap_or_else(Instant::now);
            let stream = match blobs.observe(hash).stream().await {
                Ok(s) => s,
                Err(_) => return,
            };
            let mut stream = Box::pin(stream);
            while let Some(bf) = stream.next().await {
                let bytes_done = bf.total_bytes();
                let bytes_total = bf.size();
                let complete = bf.is_complete();
                if complete || last.elapsed() >= FETCH_PROGRESS_MIN_INTERVAL {
                    last = Instant::now();
                    sink(FetchEvent::File {
                        name: name.clone(),
                        bytes_done,
                        bytes_total,
                    });
                }
                if complete {
                    break;
                }
            }
        }));
    }

    // Phase 2: pull the whole hash-sequence (already-present ranges are skipped),
    // deriving batch progress from the aggregate download stream. `Progress` is
    // cumulative request bytes (incl. locally-present) and can exceed the payload
    // `byte_size`, so clamp it; the terminal batch event below pins the total.
    let progress = downloader.download(
        HashAndFormat::hash_seq(root_hash),
        Shuffled::new(vec![provider]),
    );
    let mut stream = progress
        .stream()
        .await
        .with_context(|| format!("open collection download stream {root_hash}"))?;
    let mut last = Instant::now();
    while let Some(item) = stream.next().await {
        match item {
            DownloadProgressItem::Progress(done) => {
                if last.elapsed() >= FETCH_PROGRESS_MIN_INTERVAL {
                    last = Instant::now();
                    sink(FetchEvent::Batch {
                        bytes_done: done.min(byte_size),
                        bytes_total: byte_size,
                    });
                }
            }
            // The `AbortObserversOnDrop` guard aborts every observer as `observers`
            // drops on this early return/bail — no manual abort loop needed here.
            DownloadProgressItem::Error(e) => return Err(e.into()),
            DownloadProgressItem::DownloadError => {
                anyhow::bail!("download collection {root_hash} failed")
            }
            _ => {}
        }
    }
    // Happy path: drain (await) each observer so its terminal completion event
    // is emitted (they break on is_complete, which holds now that every child is
    // downloaded) — then the now-empty guard drops harmlessly.
    for h in observers.0.drain(..) {
        let _ = h.await;
    }
    // The final batch event always fires, at exactly the announced total.
    sink(FetchEvent::Batch {
        bytes_done: byte_size,
        bytes_total: byte_size,
    });

    // Pin the downloaded collection until the caller releases it (post-ack).
    // Between download-complete and this set the data is untagged — the 900 s
    // GC interval makes that window irrelevant in practice.
    store
        .tags()
        .set(tag, HashAndFormat::hash_seq(root_hash))
        .await
        .context("tag fetched collection")?;

    // Reconstruct the package directory from the downloaded collection.
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

/// Fetch ONLY the package manifest (`manifest.ndjson`) of the collection at
/// `root_hash` from `provider` into `dest_dir`, returning its path.
///
/// Runs phase 1 of [`fetch_collection_to_dir`] (hash-seq + collection meta), then
/// downloads just the single named `manifest.ndjson` blob and exports it — the
/// payload frames are never transferred. Used by the receiver-cancel flow (a
/// later task) to inspect a package's frames without pulling their bytes. No tag
/// is set: the manifest-only fetch is transient inspection, not a pinned package.
pub async fn fetch_manifest_to_dir(
    store: &Store,
    endpoint: &Endpoint,
    provider: EndpointId,
    root_hash: Hash,
    dest_dir: &Path,
) -> Result<PathBuf> {
    let downloader = store.downloader(endpoint);

    // Phase 1: hash-seq + collection meta (child 0) → entry names + child hashes.
    let meta_req = GetRequest::builder()
        .root(ChunkRanges::all())
        .child(0, ChunkRanges::all())
        .build(root_hash);
    downloader
        .download(meta_req, Shuffled::new(vec![provider]))
        .await
        .with_context(|| format!("download collection meta {root_hash}"))?;

    let collection = Collection::load(root_hash, store)
        .await
        .with_context(|| format!("load collection {root_hash}"))?;

    // The manifest is a named collection entry; validate its name like any other.
    let manifest_hash = collection
        .iter()
        .find(|(name, _)| name == MANIFEST_FILENAME)
        .map(|(_, hash)| *hash)
        .ok_or_else(|| {
            anyhow::anyhow!("collection {root_hash} has no {MANIFEST_FILENAME} entry")
        })?;
    validate_rel_path(MANIFEST_FILENAME)
        .with_context(|| format!("manifest entry name failed validation: {MANIFEST_FILENAME}"))?;

    // Download just the manifest blob (single-hash raw request), then export it.
    downloader
        .download(HashAndFormat::raw(manifest_hash), Shuffled::new(vec![provider]))
        .await
        .with_context(|| format!("download manifest blob {manifest_hash}"))?;

    tokio::fs::create_dir_all(dest_dir)
        .await
        .with_context(|| format!("create dest dir {}", dest_dir.display()))?;
    let target = dest_dir.join(MANIFEST_FILENAME);
    store
        .blobs()
        .export(manifest_hash, &target)
        .await
        .with_context(|| format!("export manifest -> {}", target.display()))?;

    tracing::debug!(
        provider = %provider.fmt_short(),
        root_hash = %root_hash,
        path = %target.display(),
        "manifest fetched from collection"
    );
    Ok(target)
}

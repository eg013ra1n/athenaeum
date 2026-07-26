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
use iroh_blobs::api::downloader::{DownloadProgressItem, DownloadRequest, Shuffled, SplitStrategy};
use iroh_blobs::api::{Store, TempTag};
use iroh_blobs::format::collection::Collection;
use iroh_blobs::protocol::{ChunkRanges, GetRequest};
use iroh_blobs::{Hash, HashAndFormat};
use n0_future::StreamExt as _;

use crate::package::{read_manifest, validate_rel_path, ManifestRecord, MANIFEST_FILENAME};
use crate::sharing::types::{FetchEvent, LocalFault};
use crate::sharing::{FetchSink, ProviderEvent, ProviderTelemetrySink};

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
    /// Size in bytes (the served blob's size), recorded for per-file upload
    /// attribution (Task 2.2).
    len: u64,
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
        let len = entry
            .metadata()
            .with_context(|| format!("stat {}", abs.display()))?
            .len();
        let rel = abs
            .strip_prefix(root)
            .with_context(|| format!("strip prefix {}", root.display()))?;
        // Collection names use forward slashes regardless of host separator.
        let name = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        out.push(PkgFile { abs, name, len });
    }
    if out.is_empty() {
        anyhow::bail!("package dir has no files: {}", root.display());
    }
    Ok(out)
}

/// Import every file under `pkg_dir` into `store` and assemble them into a
/// collection. Returns the collection [`Hash`] (the package `root_hash`) plus the
/// ORDERED collection entries `(rel_path, byte_size)` — in the exact order
/// [`Collection::from_iter`] stores them (the sorted-name walk order) — so the
/// provider-upload-events consumer (Task 2.2) can attribute a served child to its
/// entry by hash-seq index. Pins the collection under the deterministic `tag`
/// name so it survives garbage collection and is serveable to peers — and so
/// `release` can later delete it by that exact name.
pub async fn import_package_collection(
    store: &Store,
    pkg_dir: &Path,
    tag: &str,
) -> Result<(Hash, Vec<(String, u64)>)> {
    let files = collect_files(pkg_dir)?;
    let count = files.len();

    // Hold each child's temp tag alive until the collection is stored + tagged,
    // so nothing it references can be collected mid-assembly.
    let mut child_tags: Vec<TempTag> = Vec::with_capacity(count);
    let mut items: Vec<(String, Hash)> = Vec::with_capacity(count);
    // The ordered `(rel_path, size)` entries, in the SAME order they are handed to
    // `Collection::from_iter` (== `files`' sorted-walk order) — the by-index
    // attribution map for Task 2.2.
    let mut entries: Vec<(String, u64)> = Vec::with_capacity(count);
    for f in &files {
        let tt = store
            .blobs()
            .add_path(&f.abs)
            .temp_tag()
            .await
            .with_context(|| format!("import blob {}", f.abs.display()))?;
        items.push((f.name.clone(), tt.hash()));
        entries.push((f.name.clone(), f.len));
        child_tags.push(tt);
    }

    let hash = store_and_tag_collection(store, items, child_tags, tag).await?;
    tracing::debug!(
        path = %pkg_dir.display(),
        count,
        root_hash = %hash,
        "package imported as collection"
    );
    Ok((hash, entries))
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
) -> Result<(Hash, Vec<(String, u64)>)> {
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
    // The ordered `(rel_path, size)` entries, in the SAME (sorted) order they are
    // handed to `Collection::from_iter` — the by-index attribution map for the
    // SUBSET collection (which is what the provider iterates, so its entry order,
    // not the full manifest's, is the correct one to record; Task 2.2).
    let mut ordered: Vec<(String, u64)> = Vec::with_capacity(entries.len());
    for (name, src) in entries {
        let (tt, size) = match src {
            Src::ManifestBytes(bytes) => {
                let size = bytes.len() as u64;
                let tt = store
                    .blobs()
                    .add_bytes(bytes)
                    .temp_tag()
                    .await
                    .context("import filtered manifest blob")?;
                (tt, size)
            }
            Src::Payload(abs) => {
                let size = tokio::fs::metadata(&abs)
                    .await
                    .with_context(|| format!("stat {}", abs.display()))?
                    .len();
                let tt = store
                    .blobs()
                    .add_path(&abs)
                    .temp_tag()
                    .await
                    .with_context(|| format!("import blob {}", abs.display()))?;
                (tt, size)
            }
        };
        items.push((name.clone(), tt.hash()));
        ordered.push((name, size));
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
    Ok((hash, ordered))
}

/// The blob-store tag that GC-protects an **in-flight** collection download
/// (Task 2.3), derived from the package's permanent `tag`. The iroh-blobs
/// downloader holds no temp tag on in-flight content and GC marks live roots
/// from tags + temp-tags only, so without this a GC sweep (900 s interval)
/// collects the verified partial data of a transfer that straddles it — a
/// GB-scale fetch then restarts from ZERO instead of resuming.
///
/// Two deliberate properties:
/// - **hash_seq format at the root** (set with `HashAndFormat::hash_seq`), so the
///   GC mark phase traverses the root hash-seq and marks every child live —
///   partial children included (GC protection is blob-hash-granular: a live hash
///   retains its blob file whether complete or a verified partial).
/// - **Outside the `<role>/pkg/` namespace** the process-startup sweep clears
///   (`node::role_start`'s `delete_prefix`). A fetch killed mid-transfer must keep
///   its partial data protected ACROSS the restart until the resume re-announce
///   re-fetches, so this tag has to survive that sweep. The trade-off is orphan
///   hygiene: see [`fetch_collection_to_dir`] and the release paths in
///   [`super::node`]/[`super`] for how a stale in-flight tag is reclaimed.
pub(crate) fn in_flight_tag(permanent_tag: &str) -> String {
    format!("in-flight/{permanent_tag}")
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
///
/// ## Intentional sibling of [`fetch_collection_multi`]
///
/// [`fetch_collection_multi`] (D3 Task 1) is a deliberate COPY of this function,
/// not a refactor of it, and the two are meant to be read side by side. THIS one
/// is the load-bearing path: every personal-sync transfer and every project
/// package whose announcement predates real collection hashes falls back to it.
/// Folding both into one parameterized function would put that fallback one edit
/// away from every swarm change — the duplication IS the isolation. A fix here
/// should be considered for the sibling on purpose, with both test sets run;
/// never assume one inherits the other.
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

    // Task 2.3: GC-protect the in-flight download from here on. Phase 1 has landed
    // the root hash-seq (+ child 0) in the store, so a named hash_seq tag on the
    // root makes the GC mark phase traverse it and retain every child — even the
    // verified partial bytes of a transfer that straddles a 900 s sweep — so an
    // interrupted or slow phase-2 resumes from partial data instead of restarting
    // from zero. Phase 1's own tiny window (root+meta, before the root is known)
    // is inherently unprotected; that is fine.
    //
    // Set ONCE and, by DELIBERATE ASYMMETRY, deleted only on the SUCCESS path
    // below (after the permanent tag is set). Every non-success exit — an early
    // `?`/`bail` in phase 2, the whole future being dropped on a receiver cancel,
    // or a process kill — KEEPS this tag so the partial data survives until the
    // announce re-fires and the resume completes. Deleting here on a transient
    // fetch error would strip protection exactly when a retry is coming, defeating
    // the resume; that is why there is no drop-guard delete. Orphan hygiene for a
    // fetch that errors and is then abandoned (never retried, never cancelled)
    // rides on two reclaimers: the next successful fetch of the same package
    // reuses this exact name (`set` overwrites, then success deletes), and the
    // receiver's terminal `release` (Done via ack / Cancelled via the epilogue)
    // deletes it — see `super::node::role_release` / `super::IrohTransport::release`.
    // A process kill is not a code path, so a kill's orphan is reclaimed the same
    // two ways on the next round.
    let in_flight = in_flight_tag(tag);
    store
        .tags()
        .set(&in_flight, HashAndFormat::hash_seq(root_hash))
        .await
        .with_context(|| format!("set in-flight download tag {in_flight}"))?;

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
                        // The bitfield is the only thing that knows; see the field
                        // doc on `FetchEvent::File::complete`. An unstarted blob and
                        // an empty-but-finished one both read (0, 0) here.
                        complete,
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

    // ── D2 §3.2: everything BELOW this line is LOCAL work on data we already
    // hold — store bookkeeping and writing the collection out to our own disk.
    // Failures here are wrapped in `LocalFault`, so the receiver stamps the row
    // `Failed` ("we cannot accept this"); everything ABOVE is the transfer itself
    // and stays unmarked, so a failure there reads as a vanished peer and the row
    // parks `Waiting`. The receiver cannot make this distinction from the error
    // text — the two causes share one `Result` — so it is made here, where the
    // failing call is known.
    //
    // Pin the downloaded collection until the caller releases it (post-ack). The
    // in-flight tag has protected the data continuously since phase 1, so there is
    // NO untagged window here — set the permanent tag, THEN drop the in-flight one.
    store
        .tags()
        .set(tag, HashAndFormat::hash_seq(root_hash))
        .await
        .map_err(|e| LocalFault(anyhow::Error::new(e).context("tag fetched collection")))?;

    // Success: the permanent tag now protects the collection, so retire the
    // in-flight tag (this is the ONE path that deletes it — see the set site).
    // Best-effort: a delete failure must not fail an otherwise-complete fetch (the
    // data is safely pinned by the permanent tag); warn and continue — a later
    // release or a same-name overwrite reclaims it. Never swallow: log first.
    if let Err(e) = store.tags().delete(in_flight.as_bytes()).await {
        tracing::warn!(
            in_flight_tag = %in_flight,
            root_hash = %root_hash,
            error = %format!("{e:#}"),
            "delete in-flight download tag after successful fetch failed"
        );
    }

    // Reconstruct the package directory from the downloaded collection.
    tokio::fs::create_dir_all(dest_dir).await.map_err(|e| {
        LocalFault(anyhow::Error::new(e).context(format!("create dest dir {}", dest_dir.display())))
    })?;

    for (name, blob_hash) in collection.iter() {
        let target = dest_dir.join(name);
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                LocalFault(
                    anyhow::Error::new(e).context(format!("create dir {}", parent.display())),
                )
            })?;
        }
        store
            .blobs()
            .export(*blob_hash, &target)
            .await
            .map_err(|e| {
                LocalFault(
                    anyhow::Error::new(e).context(format!("export {name} -> {}", target.display())),
                )
            })?;
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

/// Download the collection identified by `root_hash` from MANY `providers` at
/// once into `store`, then export every entry to its `rel_path` under
/// `dest_dir` — the swarm counterpart of [`fetch_collection_to_dir`] (D3 §3.1).
///
/// ## Intentional sibling, not a refactor
///
/// This is a considered COPY of [`fetch_collection_to_dir`]; see that function's
/// doc for why the two stay apart (it is the fallback path, and entangling them
/// risks both). Read them side by side; fix them side by side.
///
/// ## What differs from the scalar fetch
///
/// - **Phase 1** (root hash-seq + collection meta) goes to the FULL provider set
///   under the default `SplitStrategy::None`. It is one tiny request, and
///   [`Shuffled`] already gives it sequential failover across every provider, so
///   splitting it buys nothing. Its per-provider attempts are therefore not
///   surfaced to `telemetry` — if every provider fails phase 1 the returned
///   `Err` is the honest signal, and a single successful meta fetch says nothing
///   useful about swarm width.
/// - **Phase 2** runs `SplitStrategy::Split`, which turns the hash-sequence into
///   one request per child blob (up to 32 in flight), each handed the full,
///   re-shuffled provider set. A child whose provider dies is re-asked of the
///   next provider for only the bytes still missing — byte-level resume, per
///   child, for free.
/// - The progress stream's per-provider items — dropped by the scalar fetch —
///   are routed into `telemetry` as [`ProviderEvent`]s.
///
/// ## Provider telemetry is BEST-EFFORT (upstream lossiness)
///
/// On iroh-blobs 0.103 the Split path does NOT guarantee delivery of every
/// per-provider item. `handle_download_split_impl` gives each child its own
/// 16-slot mpsc channel and drains the resulting stream of receivers
/// SEQUENTIALLY; a child's two events (`TryProvider` + `PartComplete`) fit that
/// buffer without ever blocking the child, so children run to completion whether
/// or not anyone has read their channel — and when the last one finishes, the
/// implementation returns and drops every receiver the drain never reached,
/// discarding their events. The faster the transfer, the more is lost:
/// instrumented localhost runs observed anywhere from 3 to 16 `TryProvider`
/// events for the same 15-child fetch, while a slow real-network transfer drains
/// far more completely.
///
/// So: `telemetry` is a SAMPLE of provider activity, sound enough to drive an
/// advisory "downloading from N sources" figure or a journal line, and never
/// sound enough for a correctness decision — do not count providers from it, do
/// not conclude a provider was unused because it never appeared, do not gate
/// retries or fallbacks on it. The transfer's own `Result` is the outcome; a
/// provider's byte counters are the ground truth about who served what.
///
/// Everything else is identical on purpose: entry-name validation before a
/// single byte touches `dest_dir`, the per-file observer tasks and their
/// abort-on-drop guard, the throttled batch progress sink, the in-flight tag
/// lifecycle (set once after phase 1, deleted ONLY on success so a killed
/// transfer resumes), and the [`LocalFault`] boundary below the permanent tag.
///
/// `providers` must be non-empty; an empty set is a caller error, not a fetch
/// that quietly succeeds against nobody.
pub async fn fetch_collection_multi(
    store: &Store,
    endpoint: &Endpoint,
    providers: Vec<EndpointId>,
    root_hash: Hash,
    tag: &str,
    dest_dir: &Path,
    byte_size: u64,
    sink: FetchSink,
    telemetry: ProviderTelemetrySink,
) -> Result<()> {
    if providers.is_empty() {
        anyhow::bail!("fetch_collection_multi {root_hash} called with an empty provider set");
    }
    let provider_count = providers.len();

    // ONE downloader reused across phase 1 + phase 2: each `store.downloader`
    // spins up a fresh actor + connection pool, so reusing it keeps the
    // connections phase 1 opened warm for the fan-out below.
    let downloader = store.downloader(endpoint);

    // Phase 1: fetch only the hash-seq (root) + collection meta (child 0) so we
    // learn the entry names and per-child hashes before the bulk transfer. Full
    // provider set, default strategy — see the doc above.
    let meta_req = GetRequest::builder()
        .root(ChunkRanges::all())
        .child(0, ChunkRanges::all())
        .build(root_hash);
    downloader
        .download(meta_req, Shuffled::new(providers.clone()))
        .await
        .with_context(|| format!("download collection meta {root_hash}"))?;

    let collection = Collection::load(root_hash, store)
        .await
        .with_context(|| format!("load collection {root_hash}"))?;

    // GC-protect the in-flight download from here on — identical contract to the
    // scalar fetch (see its long comment at the same site): a `hash_seq`-format
    // tag on the root so the GC mark phase retains every child (partials
    // included), set ONCE and deleted ONLY on the success path below, so every
    // non-success exit keeps the verified partial bytes for the next attempt.
    // Content addressing makes those bytes interchangeable across holders, so a
    // resumed swarm fetch can complete from an entirely different provider set.
    let in_flight = in_flight_tag(tag);
    store
        .tags()
        .set(&in_flight, HashAndFormat::hash_seq(root_hash))
        .await
        .with_context(|| format!("set in-flight download tag {in_flight}"))?;

    // Validate every entry name before writing anything at all — `dest_dir`
    // isn't even created yet, so a rejected entry leaves no trace on disk.
    for (name, _) in collection.iter() {
        validate_rel_path(name)
            .with_context(|| format!("collection entry name failed validation: {name}"))?;
    }

    // Per-file observers: one task per child hash, throttled (>=300 ms) with the
    // terminal event always emitted. Held in `AbortObserversOnDrop` from spawn
    // time so every exit path below aborts them instead of leaking.
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
                        complete,
                    });
                }
                if complete {
                    break;
                }
            }
        }));
    }

    // Phase 2: the fan-out. `SplitStrategy::Split` splits the hash-sequence into
    // one `GetRequest` per child and runs them buffered-unordered, each against
    // the full provider set; `Shuffled` re-shuffles per child request, which
    // load-spreads across holders for free. `Progress` is cumulative request
    // bytes (incl. locally-present) and can exceed the payload `byte_size`, so
    // clamp it; the terminal batch event below pins the total.
    let progress = downloader.download_with_opts(DownloadRequest::new(
        HashAndFormat::hash_seq(root_hash),
        Shuffled::new(providers),
        SplitStrategy::Split,
    ));
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
            // The whole point of the sibling: these two are `_ => {}` in the
            // scalar fetch. `TryProvider` fires once per (child, provider)
            // attempt — including the attempt that then finds the child already
            // local — and `ProviderFailed` once per dial/transfer failure, after
            // which the downloader moves to the next provider for that child with
            // byte-level resume. Neither is a fetch outcome; both are the raw
            // material for the "downloading from N sources" figure and the
            // per-provider journal.
            DownloadProgressItem::TryProvider { id, .. } => {
                telemetry(ProviderEvent::Trying(*id.as_bytes()));
            }
            DownloadProgressItem::ProviderFailed { id, .. } => {
                telemetry(ProviderEvent::Failed(*id.as_bytes()));
            }
            // The `AbortObserversOnDrop` guard aborts every observer as
            // `observers` drops on this early return/bail — no manual abort loop.
            DownloadProgressItem::Error(e) => return Err(e.into()),
            // In Split mode this arrives when a CHILD exhausted every provider.
            // Bail on the first one: the collection is incomplete and no later
            // child can repair it.
            DownloadProgressItem::DownloadError => {
                anyhow::bail!("multi-source download of collection {root_hash} failed")
            }
            // `PartComplete` — one child finished; the per-file observers already
            // report completion with names the caller understands.
            _ => {}
        }
    }
    // Happy path: drain (await) each observer so its terminal completion event
    // is emitted — then the now-empty guard drops harmlessly.
    for h in observers.0.drain(..) {
        let _ = h.await;
    }
    // The final batch event always fires, at exactly the announced total.
    sink(FetchEvent::Batch {
        bytes_done: byte_size,
        bytes_total: byte_size,
    });

    // ── D2 §3.2: everything BELOW this line is LOCAL work on data we already
    // hold — store bookkeeping and writing the collection out to our own disk.
    // Failures here are wrapped in `LocalFault` ("we cannot accept this");
    // everything ABOVE is the transfer itself and stays unmarked, so a failure
    // there reads as vanished peers. Identical boundary, identical reasoning, as
    // the scalar sibling.
    //
    // Pin the downloaded collection until the caller releases it. The in-flight
    // tag has protected the data continuously since phase 1, so there is NO
    // untagged window here — set the permanent tag, THEN drop the in-flight one.
    store
        .tags()
        .set(tag, HashAndFormat::hash_seq(root_hash))
        .await
        .map_err(|e| LocalFault(anyhow::Error::new(e).context("tag fetched collection")))?;

    // Success: the permanent tag now protects the collection, so retire the
    // in-flight tag (the ONE path that deletes it). Best-effort — a delete
    // failure must not fail an otherwise-complete fetch. Never swallow: log.
    if let Err(e) = store.tags().delete(in_flight.as_bytes()).await {
        tracing::warn!(
            in_flight_tag = %in_flight,
            root_hash = %root_hash,
            error = %format!("{e:#}"),
            "delete in-flight download tag after successful fetch failed"
        );
    }

    // Reconstruct the package directory from the downloaded collection.
    tokio::fs::create_dir_all(dest_dir).await.map_err(|e| {
        LocalFault(anyhow::Error::new(e).context(format!("create dest dir {}", dest_dir.display())))
    })?;

    for (name, blob_hash) in collection.iter() {
        let target = dest_dir.join(name);
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                LocalFault(
                    anyhow::Error::new(e).context(format!("create dir {}", parent.display())),
                )
            })?;
        }
        store
            .blobs()
            .export(*blob_hash, &target)
            .await
            .map_err(|e| {
                LocalFault(
                    anyhow::Error::new(e).context(format!("export {name} -> {}", target.display())),
                )
            })?;
    }

    tracing::debug!(
        providers = provider_count,
        root_hash = %root_hash,
        count = collection.len(),
        path = %dest_dir.display(),
        "collection fetched from multiple providers to package dir"
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
        .download(
            HashAndFormat::raw(manifest_hash),
            Shuffled::new(vec![provider]),
        )
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

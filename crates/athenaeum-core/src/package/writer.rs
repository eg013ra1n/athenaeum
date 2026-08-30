//! Package writer: copy payload files into a destination directory, write the
//! NDJSON manifest, and produce the [`PackageAnnounce`] that advertises it.

use std::fs;
use std::io::{Read, Write};
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

    write_manifest(dest_dir, &manifest_records)?;

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

/// Write `manifest.ndjson` into `dest_dir`: one compact JSON record per line,
/// no pretty-printing.
///
/// Split out of [`write_package_with_root_hash`] so a caller that stages its own
/// payloads — the preparation worker, which needs per-file progress and
/// cancellation while it copies — writes the identical manifest afterwards
/// instead of re-implementing the format. There is exactly one producer of this
/// file; [`super::read_manifest`] is its only reader.
pub fn write_manifest(dest_dir: &Path, records: &[ManifestRecord]) -> Result<()> {
    fs::create_dir_all(dest_dir)
        .with_context(|| format!("create package dir {}", dest_dir.display()))?;
    let manifest_path = dest_dir.join(MANIFEST_FILENAME);
    let mut buf = String::new();
    for r in records {
        let line = serde_json::to_string(r).context("serialize manifest record")?;
        buf.push_str(&line);
        buf.push('\n');
    }
    fs::write(&manifest_path, buf.as_bytes())
        .with_context(|| format!("write manifest {}", manifest_path.display()))
}

/// What [`stage_payload`] learned about a payload while staging it: the
/// full-content digest (same lowercase 16-char hex as
/// [`super::xxh3_full_file`], which the manifest record stores) and the byte
/// count actually staged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedPayload {
    pub xxh3: String,
    pub bytes: u64,
}

/// The caller's `cancelled()` predicate went true mid-stage.
///
/// A distinct type rather than a string so the preparation worker can tell a
/// user-requested stop from a real I/O failure —
/// `err.downcast_ref::<StageCancelled>()` on the `anyhow::Error` — and report
/// *cancelled* instead of *failed*.
#[derive(Debug)]
pub struct StageCancelled;

impl std::fmt::Display for StageCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("preparation cancelled")
    }
}

impl std::error::Error for StageCancelled {}

/// One-pass staging of a payload file into the package.
///
/// Reflink first: APFS / Btrfs / XFS / ReFS clone the extents in constant time
/// and zero extra disk, so the file only has to be *read* once — to hash the
/// clone. Where the filesystem cannot clone (ext4, exFAT, cross-device — a plain
/// `Err` from `reflink`, which is also what an existing `dest` returns) it falls
/// back to streaming the source: one read, one write, hashing as it goes. Either
/// way the caller pays a single pass over the bytes instead of the copy-then-hash
/// two passes `write_package` does.
///
/// The staged file is verified against `expected_size` and `dest` is removed on
/// *every* failure path — a size drift, an I/O error, a cancellation — so a
/// package directory never keeps a partial payload that its manifest claims is
/// whole. `cancelled` is consulted before the first byte and every 64 MiB;
/// a stop returns [`StageCancelled`]. `on_progress` receives the running byte
/// count, its last call equal to the file's size (a zero-byte payload reports
/// nothing — there is no byte to report).
pub fn stage_payload(
    src: &Path,
    dest: &Path,
    expected_size: u64,
    cancelled: &dyn Fn() -> bool,
    on_progress: &mut dyn FnMut(u64),
) -> Result<StagedPayload> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create payload dir {}", parent.display()))?;
    }
    let result = stage_payload_inner(src, dest, expected_size, cancelled, on_progress);
    if let Err(e) = &result {
        tracing::debug!(
            src = %src.display(),
            dest = %dest.display(),
            error = %e,
            "payload staging failed; removing partial file"
        );
        let _ = fs::remove_file(dest);
    }
    result
}

fn stage_payload_inner(
    src: &Path,
    dest: &Path,
    expected_size: u64,
    cancelled: &dyn Fn() -> bool,
    on_progress: &mut dyn FnMut(u64),
) -> Result<StagedPayload> {
    // 4 MiB reads: the size `xxh3_full_file` uses, and what the network storage
    // these libraries live on wants. Streaming xxh3 is read-size independent, so
    // the digest matches whatever the buffer.
    const CHUNK: usize = 4 * 1024 * 1024;
    const CANCEL_EVERY: u64 = 64 * 1024 * 1024;

    if cancelled() {
        return Err(StageCancelled.into());
    }

    // Staging a file onto itself would delete the user's original on the next
    // line. No caller does it (`dest` always lives under the package dir), so
    // this is a loud refusal, not a supported mode.
    if src == dest {
        anyhow::bail!("refusing to stage {} onto itself", src.display());
    }

    // `reflink` refuses an existing `to`, and a leftover from an earlier attempt
    // must not survive into this one either way.
    let _ = fs::remove_file(dest);

    let mut hasher = Xxh3::new();
    let mut buf = vec![0u8; CHUNK];
    let mut done: u64 = 0;
    let mut next_cancel_check = CANCEL_EVERY;

    let reflinked = match reflink_copy::reflink(src, dest) {
        Ok(()) => true,
        Err(e) => {
            // Not an error: the filesystem simply cannot clone. Recorded at
            // trace so a debugging session can see which branch a machine took
            // without flooding a per-file loop at info/debug.
            tracing::trace!(
                src = %src.display(),
                error = %e,
                "reflink unavailable; streaming copy instead"
            );
            false
        }
    };

    // Reflinked: read the clone (the source's bytes, already on disk) and write
    // nothing. Otherwise: read the source and write the copy.
    let read_from = if reflinked { dest } else { src };
    let mut input = fs::File::open(read_from)
        .with_context(|| format!("open {} for staging", read_from.display()))?;
    let mut output = if reflinked {
        None
    } else {
        Some(fs::File::create(dest).with_context(|| format!("create {}", dest.display()))?)
    };

    loop {
        let n = input
            .read(&mut buf)
            .with_context(|| format!("read {}", read_from.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        if let Some(out) = output.as_mut() {
            out.write_all(&buf[..n])
                .with_context(|| format!("write {}", dest.display()))?;
        }
        done += n as u64;
        on_progress(done);
        if done >= next_cancel_check {
            next_cancel_check += CANCEL_EVERY;
            if cancelled() {
                return Err(StageCancelled.into());
            }
        }
    }

    if let Some(out) = output.as_mut() {
        // Durability hint, not a correctness gate — the receiver re-verifies the
        // payload against the manifest hash — but never silently dropped: a
        // failing fsync is worth seeing when a package later reads back short.
        if let Err(e) = out.sync_data() {
            tracing::debug!(dest = %dest.display(), error = %e, "sync_data on staged payload failed");
        }
    }

    // Same integrity guard `write_package` has always applied: the staged file
    // must match the size the manifest is about to declare, else the package
    // advertises a `byte_size`/`xxh3` its own payload no longer satisfies
    // (truncated read, racing writer, wrong record).
    if done != expected_size {
        anyhow::bail!(
            "package copy size mismatch for {}: staged {} bytes, expected {}",
            src.display(),
            done,
            expected_size
        );
    }

    Ok(StagedPayload {
        xxh3: format!("{:016x}", hasher.digest()),
        bytes: done,
    })
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

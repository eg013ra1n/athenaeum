//! Package format: a portable directory of payload files plus an NDJSON
//! manifest (Stage I personal sync, task A3).
//!
//! A *package* is a self-describing on-disk bundle a peer can fetch and ingest
//! without the producer's database:
//!
//! ```text
//! <package_dir>/
//!   manifest.ndjson        one ManifestRecord per line (compact JSON, v:1)
//!   <rel_path>...          the payload files the manifest points at
//! ```
//!
//! [`write_package`] copies payload files into a destination directory, writes
//! the manifest, and returns a [`PackageAnnounce`](crate::sharing::types::PackageAnnounce)
//! describing the bundle. [`read_manifest`] parses it back and
//! [`validate_package`] re-verifies every payload against its record (existence,
//! byte size, full-content xxh3).
//!
//! Scope: no container/zip (that lives in the transport, task A5), no DB access.
//! The `root_hash` in the announce is a producer-defined placeholder here — task
//! A5 swaps in the iroh collection hash behind the same opaque string field.
//!
//! This module is intentionally ungated: it pulls no render/solver dependency
//! and compiles in the headless (`--no-default-features`) build.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Component, Path};

use anyhow::{bail, Context, Result};
use xxhash_rust::xxh3::Xxh3;

mod manifest;
mod reader;
mod writer;

#[cfg(test)]
mod tests;

pub use manifest::{ManifestRecord, PayloadKind, MANIFEST_VERSION};
pub use reader::{read_manifest, validate_package};
pub use writer::{write_package, write_package_with_root_hash, RootHashProvider};

/// Name of the manifest file inside a package directory.
pub const MANIFEST_FILENAME: &str = "manifest.ndjson";

/// Validate that `rel_path` is safe to join onto a base directory: relative,
/// with no root/prefix component and no `..` (parent-dir escape).
///
/// Shared by every path in the codebase that joins an **untrusted, wire- or
/// record-supplied** relative path onto a base directory — a value must never
/// be allowed to resolve outside that base:
/// - [`writer::write_package_with_root_hash`] validates each manifest
///   record's `rel_path` before copying the source file to `dest_dir.join(rel)`.
/// - `sharing::iroh::blobs::fetch_collection_to_dir` validates each downloaded
///   collection's entry name before exporting to `dest_dir.join(name)` — the
///   collection is peer-supplied, so its names are exactly as untrusted as a
///   manifest record's `rel_path`.
///
/// A single check, not duplicated: `Path::join` on Unix silently **replaces**
/// the base when the joined component is absolute (e.g.
/// `dest.join("/etc/x")` == `/etc/x`), and does nothing to stop `../../x`
/// climbing out of `dest` — both must be rejected before the join, not after.
pub fn validate_rel_path(rel_path: &str) -> Result<()> {
    let rel = Path::new(rel_path);
    if rel
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        bail!(
            "rel_path must be relative with no '..'/root components: {}",
            rel_path
        );
    }
    Ok(())
}

/// Full-content xxh3-64 of a file, lowercase 16-char hex.
///
/// Deliberately distinct from [`crate::duplicates::compute_xxhash`], which
/// SAMPLES (first/middle/last 512 KB) for fast move-verify. A package manifest
/// must catch corruption anywhere in the file, so this streams every byte.
pub fn xxh3_full_file(path: &Path) -> Result<String> {
    let file = File::open(path).with_context(|| format!("open {} for hashing", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut hasher = Xxh3::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader
            .read(&mut buf)
            .with_context(|| format!("read {} for hashing", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:016x}", hasher.digest()))
}

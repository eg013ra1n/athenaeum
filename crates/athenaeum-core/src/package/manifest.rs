//! Manifest schema for a shareable package (v1).
//!
//! A package's manifest is an NDJSON file — exactly one [`ManifestRecord`] per
//! line, compact (never pretty-printed), each stamped with `v: 1`. The record
//! is the portable snapshot of one payload file: identity, a full-content xxh3,
//! and the catalog metadata a receiver needs to ingest it without the source
//! database.
//!
//! Forward compatibility is by construction: the struct does NOT use
//! `#[serde(deny_unknown_fields)]`, so a newer producer that appends fields
//! round-trips through an older reader (unknown keys are ignored).

use serde::{Deserialize, Serialize};

/// Current manifest schema version, stamped into [`ManifestRecord::v`].
pub const MANIFEST_VERSION: u32 = 1;

/// What a payload file is, from the producer's point of view. Drives the
/// receiver's ingestion path; `Other` is the forward-compatible catch-all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PayloadKind {
    RawFrame,
    CalibratedLight,
    Master,
    Other,
}

/// One NDJSON line: the portable record for a single payload file in a package.
///
/// `frame_meta` carries a serialized `models::Frame` snapshot and `analysis` an
/// optional `frame_analysis` summary — both kept as opaque `serde_json::Value`
/// so this layer never has to track the evolving shape of those catalog types.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestRecord {
    /// Schema version; always [`MANIFEST_VERSION`] for records this app writes.
    pub v: u32,
    pub frame_uuid: String,
    pub origin_catalog_uuid: String,
    /// Originating peer identity, hex-encoded NodeId (ed25519 public key).
    pub origin_device: String,
    pub payload_kind: PayloadKind,
    /// Path of the payload file relative to the package root, forward slashes.
    pub rel_path: String,
    pub byte_size: u64,
    /// Full-content xxh3-64 (16-char hex) of the payload file. NOT the sampling
    /// hash from `duplicates` — see [`crate::package::xxh3_full_file`].
    pub xxh3: String,
    /// `models::Frame` row snapshot, serialized. Opaque here.
    pub frame_meta: serde_json::Value,
    /// `frame_analysis` summary when the source had one; else `None`.
    pub analysis: Option<serde_json::Value>,
    pub app_version: String,
}

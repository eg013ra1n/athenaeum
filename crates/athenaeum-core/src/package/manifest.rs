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

/// Stage-II project provenance stamp (slice 4). Appended, optional — absent
/// for personal-sync packages; forward-compatible (manifest is JSON).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectStamp {
    pub project_id: String,
    /// HUB package uuid (announcement correlation key — audit B1). The wire
    /// `PackageId` is engine-minted per serve and only correlates acks.
    pub package_id: String,
    /// Threshold-set version the frames passed (spec §4 Q4 stamp).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thresholds_version: Option<i64>,
    /// Light-calibration engine version of the payloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cal_engine_version: Option<i64>,
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
    /// Stage-II collab provenance (slice 4). Appended LAST, optional — absent on
    /// personal-sync records so v1 lines still parse (forward-compatible JSON).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectStamp>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal, fully-populated [`ManifestRecord`] fixture for the manifest
    /// schema tests (no disk I/O — synthetic identity + metadata).
    fn sample_record() -> ManifestRecord {
        ManifestRecord {
            v: MANIFEST_VERSION,
            frame_uuid: "frame-uuid-0001".to_string(),
            origin_catalog_uuid: "catalog-uuid-0001".to_string(),
            origin_device: "ab".repeat(32),
            payload_kind: PayloadKind::RawFrame,
            rel_path: "M42/L_0001.fits".to_string(),
            byte_size: 4096,
            xxh3: "0011223344556677".to_string(),
            frame_meta: serde_json::json!({ "object": "M42" }),
            analysis: None,
            app_version: "0.5.0".to_string(),
            project: None,
        }
    }

    #[test]
    fn project_stamp_roundtrips_and_absent_field_parses() {
        let mut r = sample_record(); // the fixture helper added in this step
        r.project = Some(ProjectStamp {
            project_id: "p-1".into(),
            package_id: "pkg-1".into(),
            thresholds_version: Some(3),
            cal_engine_version: Some(1),
        });
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"projectId\":\"p-1\""));
        let back: ManifestRecord = serde_json::from_str(&s).unwrap();
        assert_eq!(back.project, r.project);
        // v1 personal-sync line (no `project` key) still parses:
        let legacy = s.replace(
            &format!(
                ",\"project\":{}",
                serde_json::to_string(r.project.as_ref().unwrap()).unwrap()
            ),
            "",
        );
        let old: ManifestRecord = serde_json::from_str(&legacy).unwrap();
        assert!(old.project.is_none());
    }
}

//! Shared `manifest.json` model for the density-tier catalog (written by
//! `catalog-builder`, read by the app's download path).

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ManifestTier {
    pub density: u32,
    pub zip: String,
    pub sha256: String,
    pub dir: String,
    pub size_bytes: u64,
    pub min_fov_deg: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub catalog_epoch: f64,
    pub tiers: Vec<ManifestTier>,
}

impl Manifest {
    /// Parse a `manifest.json` byte slice.
    pub fn from_json_slice(bytes: &[u8]) -> anyhow::Result<Self> {
        Ok(serde_json::from_slice(bytes)?)
    }
}

/// One quad-index pack in `index_manifest.json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IndexManifestPack {
    /// Short pack name (`base`, `narrow`), the id the download command takes.
    pub name: String,
    pub zip: String,
    pub sha256: String,
    /// Directory the zip holds `quads.smaq` in, and the local install dir name.
    pub dir: String,
    pub size_bytes: u64,
    /// `QuadIndex::min_fov_deg()` of the pack.
    pub min_fov_deg: f64,
}

/// `index_manifest.json`: the published quad-index packs (written by
/// `catalog-builder publish-index`, read by the app's download path).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IndexManifest {
    pub version: u32,
    /// `solvemyastro::index::FORMAT_VERSION` the packs were built with; the
    /// app skips packs it cannot open.
    pub format: u32,
    pub catalog_epoch: f64,
    pub packs: Vec<IndexManifestPack>,
}

impl IndexManifest {
    /// Parse an `index_manifest.json` byte slice.
    pub fn from_json_slice(bytes: &[u8]) -> anyhow::Result<Self> {
        Ok(serde_json::from_slice(bytes)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_manifest_shape() {
        let json = br#"{
          "version": 1, "catalog_epoch": 2016.0,
          "tiers": [
            {"density":500,"zip":"tier_500.zip","sha256":"tier_500.zip.sha256",
             "dir":"tier_500","size_bytes":578617584,"min_fov_deg":0.6},
            {"density":2000,"zip":"tier_2000.zip","sha256":"tier_2000.zip.sha256",
             "dir":"tier_2000","size_bytes":1733296370,"min_fov_deg":0.3}
          ]
        }"#;
        let m = Manifest::from_json_slice(json).unwrap();
        assert_eq!(m.version, 1);
        assert_eq!(m.tiers.len(), 2);
        assert_eq!(m.tiers[0].density, 500);
        assert_eq!(m.tiers[1].min_fov_deg, 0.3);
        // round-trips
        let back = Manifest::from_json_slice(&serde_json::to_vec(&m).unwrap()).unwrap();
        assert_eq!(back.tiers[0].zip, "tier_500.zip");
    }

    #[test]
    fn parses_index_manifest_shape() {
        let json = br#"{
          "version": 1, "format": 3, "catalog_epoch": 2016.0,
          "packs": [
            { "name": "base",   "zip": "index_base.zip",   "sha256": "index_base.zip.sha256",
              "dir": "base",   "size_bytes": 147079024, "min_fov_deg": 1.3 },
            { "name": "narrow", "zip": "index_narrow.zip", "sha256": "index_narrow.zip.sha256",
              "dir": "narrow", "size_bytes": 235801384, "min_fov_deg": 0.65 }
          ]
        }"#;
        let m = IndexManifest::from_json_slice(json).unwrap();
        assert_eq!((m.version, m.format), (1, 3));
        assert_eq!(m.packs.len(), 2);
        assert_eq!(m.packs[0].name, "base");
        assert_eq!(m.packs[1].min_fov_deg, 0.65);
        assert_eq!(m.packs[1].size_bytes, 235_801_384);
        let back = IndexManifest::from_json_slice(&serde_json::to_vec(&m).unwrap()).unwrap();
        assert_eq!(back.packs[1].zip, "index_narrow.zip");
        assert!(IndexManifest::from_json_slice(br#"{"version":1}"#).is_err());
    }
}

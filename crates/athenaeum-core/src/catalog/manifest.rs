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
}

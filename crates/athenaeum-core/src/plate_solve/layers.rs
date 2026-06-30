//! Discover the installed density-tier catalog dirs (or a legacy single cache).
//!
//! The plate-solver consumes an additive stack of disjoint density tiers,
//! installed as `tier_<density>/stars.smac` under the catalog root. This
//! module finds those dirs (ascending density = base → deepest) so the caller
//! can open each as a `StarCache` and pass `Caches::layered(..)` to the solver.
//! A legacy single `stars.smac` directly under the root is supported as a
//! 1-layer fallback so old installs keep solving.

use std::path::{Path, PathBuf};

/// Ordered `tier_<density>/` dirs (ascending density) under `catalog_root`.
///
/// Falls back to `[catalog_root]` when no tiers exist but a legacy
/// `stars.smac` is present directly under the root. Returns an empty `Vec`
/// when neither tiers nor a legacy cache are found (caller surfaces the
/// "catalog not installed" error).
pub fn discover_layers(catalog_root: &Path) -> Vec<PathBuf> {
    let mut tiers: Vec<(u32, PathBuf)> = match std::fs::read_dir(catalog_root) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().to_str()?.to_string();
                let density: u32 = name.strip_prefix("tier_")?.parse().ok()?;
                if e.path().join("stars.smac").is_file() {
                    Some((density, e.path()))
                } else {
                    None
                }
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    tiers.sort_by_key(|(d, _)| *d);
    if !tiers.is_empty() {
        return tiers.into_iter().map(|(_, p)| p).collect();
    }
    if catalog_root.join("stars.smac").is_file() {
        return vec![catalog_root.to_path_buf()];
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_tiers_in_density_order_else_legacy() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for d in ["tier_2000", "tier_500", "tier_8000"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
            std::fs::write(root.join(d).join("stars.smac"), b"x").unwrap();
        }
        // A `tier_*` dir without stars.smac must be ignored.
        std::fs::create_dir_all(root.join("tier_9999")).unwrap();
        let got = discover_layers(root);
        let names: Vec<_> = got
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(names, vec!["tier_500", "tier_2000", "tier_8000"]);

        // Legacy fallback: a bare stars.smac, no tiers.
        let leg = tempfile::tempdir().unwrap();
        std::fs::write(leg.path().join("stars.smac"), b"x").unwrap();
        assert_eq!(discover_layers(leg.path()), vec![leg.path().to_path_buf()]);

        // Neither tiers nor legacy → empty.
        let empty = tempfile::tempdir().unwrap();
        assert!(discover_layers(empty.path()).is_empty());
    }
}

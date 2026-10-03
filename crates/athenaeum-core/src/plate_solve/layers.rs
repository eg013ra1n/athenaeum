//! Discover the installed density-tier catalog dirs (or a legacy single cache).
//!
//! The plate-solver consumes an additive stack of disjoint density tiers,
//! installed as `tier_<density>/stars.smac` under the catalog root. This
//! module finds those dirs (ascending density = base → deepest) so the caller
//! can open each as a `StarCache` and pass `Caches::layered(..)` to the solver.
//! A legacy single `stars.smac` directly under the root is supported as a
//! 1-layer fallback so old installs keep solving.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

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

/// Why the plate-solve star catalog could not be opened.
#[derive(Debug)]
pub enum CatalogOpenError {
    /// No tier and no legacy `stars.smac` under `root`.
    NotInstalled { root: PathBuf },
    /// A tier exists, but it could not be opened or read.
    Open { dir: PathBuf, error: String },
}

impl std::fmt::Display for CatalogOpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CatalogOpenError::NotInstalled { root } => {
                write!(
                    f,
                    "the plate-solve star catalog is not installed under {}",
                    root.display()
                )
            }
            CatalogOpenError::Open { dir, error } => {
                write!(f, "Failed to open star cache at {}: {error}", dir.display())
            }
        }
    }
}

impl std::error::Error for CatalogOpenError {}

/// `<app-data>/catalogs/smac_gaia`, where the hosts install the catalog.
pub fn catalog_root(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("catalogs").join("smac_gaia")
}

/// Open the additive tier stack under [`catalog_root`] (base -> deepest), or
/// a legacy single cache. This is the one opener: both hosts and a stacking
/// run call it. Opens fresh on each call; the mmap is cheap.
pub fn open_catalog_layers(
    app_data_dir: &Path,
) -> Result<Vec<Arc<solvemyastro::StarCache>>, CatalogOpenError> {
    let root = catalog_root(app_data_dir);
    let dirs = discover_layers(&root);
    if dirs.is_empty() {
        return Err(CatalogOpenError::NotInstalled { root });
    }
    let mut layers = Vec::with_capacity(dirs.len());
    for dir in &dirs {
        let cache = solvemyastro::StarCache::open(dir).map_err(|e| CatalogOpenError::Open {
            dir: dir.clone(),
            error: e.to_string(),
        })?;
        layers.push(Arc::new(cache));
    }
    tracing::debug!(count = layers.len(), path = %root.display(), "opened catalog layers");
    Ok(layers)
}

/// The installed catalog's identity, for a cached solve's hash: every tier's
/// directory name (`legacy` for a bare root cache), with its `stars.smac`
/// size and mtime in seconds, in tier order. Stat only, no read. It walks the
/// tiers apart from [`open_catalog_layers`]: a catalog swapped between the
/// two walks could pair the new identity with the old layers for one run,
/// which is negligible (review M15).
pub fn catalog_identity(app_data_dir: &Path) -> Result<String, CatalogOpenError> {
    let root = catalog_root(app_data_dir);
    let dirs = discover_layers(&root);
    if dirs.is_empty() {
        return Err(CatalogOpenError::NotInstalled { root });
    }
    let mut parts = Vec::with_capacity(dirs.len());
    for dir in &dirs {
        let meta =
            std::fs::metadata(dir.join("stars.smac")).map_err(|e| CatalogOpenError::Open {
                dir: dir.clone(),
                error: e.to_string(),
            })?;
        let mtime = match meta.modified().map(|t| t.duration_since(UNIX_EPOCH)) {
            Ok(Ok(d)) => d.as_secs(),
            Ok(Err(e)) => {
                tracing::warn!(path = %dir.join("stars.smac").display(), error = %e, "catalog tier mtime unavailable");
                0
            }
            Err(e) => {
                tracing::warn!(path = %dir.join("stars.smac").display(), error = %e, "catalog tier mtime unavailable");
                0
            }
        };
        let name = if *dir == root {
            "legacy".to_string()
        } else {
            dir.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        parts.push(format!("{name}:{}:{mtime}", meta.len()));
    }
    Ok(parts.join(","))
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

    /// Plan 6b-2 C14: the catalog's identity names every tier with its
    /// `stars.smac` size and mtime, so replacing a tier moves it. With no
    /// catalog there is no identity and nothing to open.
    #[test]
    fn the_catalog_identity_follows_every_tier_file() {
        let app = tempfile::tempdir().unwrap();
        let root = catalog_root(app.path());
        assert_eq!(root, app.path().join("catalogs").join("smac_gaia"));
        assert!(
            matches!(catalog_identity(app.path()), Err(CatalogOpenError::NotInstalled { root: r }) if r == root)
        );
        assert!(matches!(
            open_catalog_layers(app.path()),
            Err(CatalogOpenError::NotInstalled { .. })
        ));
        for (tier, bytes) in [("tier_500", &b"x"[..]), ("tier_2000", &b"yy"[..])] {
            std::fs::create_dir_all(root.join(tier)).unwrap();
            std::fs::write(root.join(tier).join("stars.smac"), bytes).unwrap();
        }
        let id = catalog_identity(app.path()).unwrap();
        let parts: Vec<&str> = id.split(',').collect();
        assert_eq!(parts.len(), 2, "{id}");
        assert!(parts[0].starts_with("tier_500:1:"), "{id}");
        assert!(parts[1].starts_with("tier_2000:2:"), "{id}");
        std::fs::write(root.join("tier_500").join("stars.smac"), b"xyz").unwrap();
        let moved = catalog_identity(app.path()).unwrap();
        assert_ne!(moved, id, "a replaced tier moves the identity");
        assert!(moved.starts_with("tier_500:3:"), "{moved}");
    }

    /// Plan 6b-2 C14 (fix round 1): a replaced tier file with the same size
    /// still moves the identity (mtime only), and a bare root cache is named
    /// `legacy`.
    #[test]
    fn the_catalog_identity_follows_mtime_and_names_a_legacy_layer() {
        let app = tempfile::tempdir().unwrap();
        let root = catalog_root(app.path());
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("stars.smac");
        std::fs::write(&file, b"abc").unwrap();
        let id = catalog_identity(app.path()).unwrap();
        assert!(id.starts_with("legacy:3:"), "{id}");
        let f = std::fs::OpenOptions::new().write(true).open(&file).unwrap();
        let later = std::fs::metadata(&file).unwrap().modified().unwrap()
            + std::time::Duration::from_secs(120);
        f.set_modified(later).unwrap();
        drop(f);
        let moved = catalog_identity(app.path()).unwrap();
        assert!(moved.starts_with("legacy:3:"), "size unchanged: {moved}");
        assert_ne!(moved, id, "an mtime-only change moves the identity");
    }

    /// Plan 6b-2 C14 (fix round 1): a tier that cannot be opened is an `Open`
    /// error naming the first tier in order.
    #[test]
    fn open_catalog_layers_names_the_first_unopenable_tier() {
        let app = tempfile::tempdir().unwrap();
        let root = catalog_root(app.path());
        for tier in ["tier_2000", "tier_500"] {
            std::fs::create_dir_all(root.join(tier)).unwrap();
            std::fs::write(root.join(tier).join("stars.smac"), b"x").unwrap();
        }
        match open_catalog_layers(app.path()) {
            Err(CatalogOpenError::Open { dir, .. }) => assert_eq!(dir, root.join("tier_500")),
            Err(e) => panic!("expected Open, got {e:?}"),
            Ok(_) => panic!("a one-byte stars.smac must not open"),
        }
    }
}

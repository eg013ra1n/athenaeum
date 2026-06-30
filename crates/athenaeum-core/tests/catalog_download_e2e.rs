//! Local end-to-end test for `download_catalog_layers`: needs a static HTTP host
//! serving a `publish/` tree (manifest.json + tier_*.zip + .sha256) and
//! `ATHENAEUM_CATALOG_BASE_URL` pointing at it.
//!
//! Run:
//! ```sh
//! (cd ~/catalog_out/publish && python3 -m http.server 8765) &
//! ATHENAEUM_CATALOG_BASE_URL=http://localhost:8765/ \
//!   cargo test -p athenaeum-core --test catalog_download_e2e -- --ignored --nocapture
//! ```
//!
//! Exercises the real glue end-to-end: manifest fetch, tier selection, resumable
//! download, SHA-256 verify, tier-prefix extract, install detection, idempotency,
//! and per-tier status.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use athenaeum_core::catalog::gaia_prebuilt::{download_catalog_layers, tier_status};

#[test]
#[ignore = "needs a local catalog host + ATHENAEUM_CATALOG_BASE_URL"]
fn download_catalog_layers_installs_and_is_idempotent() {
    if std::env::var("ATHENAEUM_CATALOG_BASE_URL").is_err() {
        println!("SKIP: set ATHENAEUM_CATALOG_BASE_URL to a host serving publish/");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let app_data = tmp.path();
    let cancel = Arc::new(AtomicBool::new(false));

    // Download the base tier (density 500) — exercises manifest fetch, tier
    // selection, resumable download, SHA-256 verify, extract_tier_zip, smac_present.
    let root = download_catalog_layers(app_data, 500, cancel.clone(), &|_p| {})
        .expect("download tier 500");

    let tier_dir = root.join("tier_500");
    let cache = solvemyastro::StarCache::open(&tier_dir).expect("open tier_500 cache");
    assert!(cache.star_count() > 0, "tier_500 should carry stars");
    println!(
        "installed tier_500: {} stars at {}",
        cache.star_count(),
        tier_dir.display()
    );

    // The manifest is cached locally for offline status.
    assert!(root.join("manifest.json").is_file(), "manifest cached on disk");

    // Idempotent: a second call at the same target re-downloads nothing.
    let root2 = download_catalog_layers(app_data, 500, cancel, &|_p| {})
        .expect("idempotent re-run");
    assert_eq!(root, root2);

    // Status reflects the install (and lists the not-yet-installed deeper tiers).
    let status = tier_status(app_data);
    let t500 = status
        .iter()
        .find(|t| t.density == 500)
        .expect("tier 500 present in status");
    assert!(t500.installed, "tier_500 must read as installed");
    assert!(t500.star_count > 0);
    println!(
        "tier_status: {} declared tier(s); tier_500 installed={} stars={}",
        status.len(),
        t500.installed,
        t500.star_count
    );
}

//! Gaia DR3 (G ≤ 16) all-sky catalog ingest.
//!
//! Mirrors the [`super::tycho2`] pipeline, but Gaia is far too large to bulk
//! download (the `gaia_source` repo is HEALPix-partitioned, *not*
//! magnitude-partitioned — a G≤16 subset would still mean ~600 GB of
//! transfer). Instead we extract via the ESA TAP **async** service, tiled by
//! `source_id` range so the server filters `phot_g_mean_mag < 16` and
//! projects only the 5 columns we store. Each tile is an independent,
//! resumable checkpoint.
//!
//! `source_id` encodes position: the HEALPix level-*n* index is
//! `source_id / (2^35 · 4^(12−n))`. We tile at **level 3** (nested):
//! `12 · 4^3 = 768` pixels, divisor `2^35 · 4^9 = 2^53`. At G≤16 that is
//! ≈390 k rows/tile — comfortably under the 3 M-row anonymous async cap.
//!
//! Output goes to `catalogs/gaia_dr3/` in the existing depth-6 HEALPix
//! [`StarRecord`] format; [`crate::catalog::CatalogEngine::with_catalog_dir`]
//! auto-discovers it and `cone_search` prefers it (epoch 2016.0) over
//! Tycho-2 with no solver changes.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};

/// ESA Gaia archive TAP async endpoint.
pub const GAIA_TAP_ASYNC: &str = "https://gea.esac.esa.int/tap-server/tap/async";

/// Magnitude cut (Gaia G, Vega) applied server-side.
pub const GAIA_MAG_LIMIT: f32 = 16.0;

/// HEALPix level used to tile the `source_id` space for TAP extraction.
pub const GAIA_HEALPIX_LEVEL: u32 = 3;

/// Number of nested HEALPix level-3 pixels: `12 · 4^3`.
pub const GAIA_TILE_COUNT: u64 = 768;

/// `source_id` span per level-3 tile: `2^35 · 4^(12−3)` = `2^53`.
pub const SOURCE_ID_TILE_SPAN: u64 = 1 << 53;

/// Progress callback for the query and conversion phases (mirrors
/// [`super::tycho2::Tycho2Progress`]).
pub enum GaiaProgress {
    /// One TAP tile finished: `tile` of `total_tiles`, `stars` parsed from it.
    Querying {
        tile: u64,
        total_tiles: u64,
        stars: usize,
    },
    /// Finalizing the on-disk HEALPix-6 catalog.
    Converting {
        stars_processed: usize,
        total_stars: usize,
    },
    Complete {
        total_stars: usize,
    },
    Error(String),
}

/// Inclusive `source_id` range `[lo, hi]` covered by level-3 `tile`
/// (`0..GAIA_TILE_COUNT`). Tiles tile the `source_id` space contiguously.
pub fn tile_source_id_range(tile: u64) -> (u64, u64) {
    let lo = tile * SOURCE_ID_TILE_SPAN;
    let hi = (tile + 1) * SOURCE_ID_TILE_SPAN - 1;
    (lo, hi)
}

/// Build the ADQL for one level-3 tile: server-side magnitude cut + the
/// 5 columns we store, constrained to the tile's `source_id` range (which
/// is the spatial partition — `source_id BETWEEN` is primary-key indexed,
/// so this is the fast form).
pub fn tile_adql(tile: u64) -> String {
    let (lo, hi) = tile_source_id_range(tile);
    format!(
        "SELECT ra,dec,phot_g_mean_mag,pmra,pmdec FROM gaiadr3.gaia_source \
         WHERE phot_g_mean_mag < {} AND source_id BETWEEN {lo} AND {hi}",
        GAIA_MAG_LIMIT as u32
    )
}

/// Poll cadence and a generous safety cap so a stuck job can never spin
/// forever (ESA anon async job timeout is 90 min).
const POLL_INTERVAL: Duration = Duration::from_secs(5);
const MAX_POLLS: u32 = 90 * 60 / 5;

/// A blocking HTTP client configured for ESA TAP (long timeout, polite UA).
pub fn tap_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(600))
        .user_agent("athenaeum-catalog-ingest (astrophotography catalog builder)")
        .build()
        .context("build TAP HTTP client")
}

/// Submit an async ADQL job (`PHASE=RUN`) and return the job resource URL.
/// ESA responds 303 → the job resource; reqwest follows it, so the final
/// response URL is the job URL.
pub fn submit_tap_job(client: &reqwest::blocking::Client, adql: &str) -> Result<String> {
    let resp = client
        .post(GAIA_TAP_ASYNC)
        .form(&[
            ("REQUEST", "doQuery"),
            ("LANG", "ADQL"),
            ("FORMAT", "csv"),
            ("PHASE", "RUN"),
            ("QUERY", adql),
        ])
        .send()
        .context("submit TAP job")?;
    if !resp.status().is_success() {
        anyhow::bail!("TAP submit returned HTTP {}", resp.status());
    }
    let job_url = resp.url().as_str().trim_end_matches('/').to_string();
    if !job_url.contains("/async/") {
        anyhow::bail!("unexpected TAP job URL: {job_url}");
    }
    Ok(job_url)
}

/// Poll `{job}/phase` until terminal. Ok(()) on `COMPLETED`; Err on
/// `ERROR`/`ABORTED`, on the safety cap, or on cancel.
pub fn poll_job(
    client: &reqwest::blocking::Client,
    job_url: &str,
    cancel: &Arc<AtomicBool>,
) -> Result<()> {
    for _ in 0..MAX_POLLS {
        if cancel.load(Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        let phase = client
            .get(format!("{job_url}/phase"))
            .send()
            .context("poll TAP phase")?
            .text()
            .context("read TAP phase")?;
        match phase.trim() {
            "COMPLETED" => return Ok(()),
            "ERROR" | "ABORTED" => {
                anyhow::bail!("TAP job {job_url} ended in phase {}", phase.trim())
            }
            _ => std::thread::sleep(POLL_INTERVAL),
        }
    }
    anyhow::bail!("TAP job {job_url} did not complete within the poll cap")
}

/// Fetch the completed job's CSV result body.
pub fn fetch_job_csv(client: &reqwest::blocking::Client, job_url: &str) -> Result<String> {
    let resp = client
        .get(format!("{job_url}/results/result"))
        .send()
        .context("fetch TAP result")?;
    if !resp.status().is_success() {
        anyhow::bail!("TAP result returned HTTP {}", resp.status());
    }
    resp.text().context("read TAP result body")
}

/// Placeholder so the module is usable before later tasks land; real
/// pipeline is `download_gaia_dr3` / `setup_gaia_dr3_catalog` (Tasks 3–5).
pub fn setup_gaia_dr3_catalog(
    _app_data_dir: &Path,
    _cancel_flag: Arc<AtomicBool>,
    _progress: &dyn Fn(GaiaProgress),
) -> Result<PathBuf> {
    anyhow::bail!("gaia_dr3 ingest not yet implemented (plan Tasks 2–5)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_range_partitions_source_id_space() {
        // Tile 0 starts at 0.
        assert_eq!(tile_source_id_range(0).0, 0);
        // Span is exactly 2^53.
        let (lo0, hi0) = tile_source_id_range(0);
        assert_eq!(hi0 - lo0 + 1, SOURCE_ID_TILE_SPAN);
        assert_eq!(SOURCE_ID_TILE_SPAN, 9_007_199_254_740_992);

        // Contiguous + non-overlapping across all 768 tiles.
        for t in 0..GAIA_TILE_COUNT - 1 {
            let (_, hi) = tile_source_id_range(t);
            let (next_lo, _) = tile_source_id_range(t + 1);
            assert_eq!(hi + 1, next_lo, "gap/overlap between tile {t} and {}", t + 1);
        }

        // 768 tiles cover [0, 768·2^53).
        let (last_lo, last_hi) = tile_source_id_range(GAIA_TILE_COUNT - 1);
        assert_eq!(last_lo, (GAIA_TILE_COUNT - 1) * SOURCE_ID_TILE_SPAN);
        assert_eq!(last_hi + 1, GAIA_TILE_COUNT * SOURCE_ID_TILE_SPAN);
        // …and stay within the u64 source_id space (768·2^53 ≈ 6.9e18 < 2^63).
        assert!(GAIA_TILE_COUNT * SOURCE_ID_TILE_SPAN < (1u64 << 63));
    }

    #[test]
    fn adql_is_well_formed() {
        let q0 = tile_adql(0);
        assert!(q0.contains("FROM gaiadr3.gaia_source"));
        assert!(q0.contains("phot_g_mean_mag < 16"));
        assert!(
            q0.contains("source_id BETWEEN 0 AND 9007199254740991"),
            "tile 0 range wrong: {q0}"
        );
        // Tile 1's lower bound is exactly 2^53.
        let q1 = tile_adql(1);
        assert!(
            q1.contains("BETWEEN 9007199254740992 AND "),
            "tile 1 lower bound wrong: {q1}"
        );
        // Only the 5 stored columns are projected.
        assert!(q0.starts_with("SELECT ra,dec,phot_g_mean_mag,pmra,pmdec FROM"));
    }

    #[test]
    fn level3_has_768_pixels() {
        // Nested HEALPix: 12 · 4^level.
        assert_eq!(12 * 4u64.pow(GAIA_HEALPIX_LEVEL), GAIA_TILE_COUNT);
    }
}

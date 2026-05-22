//! Gaia DR3 (G ≤ 19) all-sky catalog ingest.
//!
//! Mirrors the [`super::tycho2`] pipeline, but Gaia is far too large to bulk
//! download. Instead we extract via the ESA TAP **async** service, tiled by
//! `source_id` range so the server filters `phot_g_mean_mag < 19` and
//! projects `ra,dec,phot_g_mean_mag,pmra,pmdec,ruwe`. Each tile is an
//! independent, resumable checkpoint.
//!
//! `source_id` encodes position: the HEALPix level-*n* index is
//! `source_id / (2^35 · 4^(12−n))`. We tile at **level 5** (nested):
//! `12 · 4^5 = 12 288` pixels, divisor `2^35 · 4^7 = 2^49`. At G≤19 that is
//! ≈73 k rows/tile average — the densest galactic-plane tiles stay under
//! the ~3 M-row anonymous async cap (a level-3 tiling would not at this
//! depth). The full ingest is ~900 M sources, ~3–4 days of TAP traffic.
//!
//! ## Persistent raw-CSV cache
//!
//! Each tile's verbatim ESA CSV is kept forever in `<scratch>/raw_csv/`.
//! Nothing deletes it. A re-run reads the cached CSV instead of re-querying
//! TAP, so a re-bin (e.g. a changed [`GAIA_RUWE_MAX`] threshold or a record
//! format change) costs zero ESA traffic. The expensive ~4–8 h download
//! happens exactly once.
//!
//! ## Quality filtering
//!
//! `phot_g_mean_mag < 19` is the only server-side filter (it bounds size).
//! RUWE is projected but filtered **locally** at bin time in
//! [`parse_gaia_csv_row`] against [`GAIA_RUWE_MAX`] — keeping it out of the
//! `WHERE` clause makes the cached CSVs a full superset.
//!
//! Output goes to `catalogs/gaia_dr3/` in the existing depth-6 HEALPix
//! [`StarRecord`] format; [`crate::catalog::CatalogEngine::with_catalog_dir`]
//! auto-discovers it and `cone_search` prefers it (epoch 2016.0) over
//! Tycho-2 with no solver changes.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};

use super::binary_format::{self, StarRecord};
use super::healpix;

/// ESA Gaia archive TAP async endpoint.
pub const GAIA_TAP_ASYNC: &str = "https://gea.esac.esa.int/tap-server/tap/async";

/// Magnitude cut (Gaia G, Vega) applied server-side.
pub const GAIA_MAG_LIMIT: f32 = 19.0;

/// RUWE (Renormalised Unit Weight Error) ceiling, applied locally at bin
/// time. Gaia DR3 sources with RUWE >= 1.4 have unreliable astrometric
/// solutions (typically unresolved binaries — the catalog position itself
/// is biased) and are dropped at ingest. Rows with no RUWE value (Gaia
/// 2-parameter solutions, which have no RUWE by construction) are kept — a
/// missing RUWE is not a quality failure. Changing this threshold only
/// requires re-binning the cached raw CSVs, never a re-download.
pub const GAIA_RUWE_MAX: f64 = 1.4;

/// HEALPix level used to tile the `source_id` space for TAP extraction.
///
/// Level 5 (not 3) because at the `G < 19` depth a level-3 tile in the
/// galactic plane would hold several million rows — past ESA's anonymous
/// async per-query cap. Level 5 gives ~73 k rows/tile average and keeps the
/// densest tiles comfortably under cap. (Tiling level only chunks the
/// *download*; the binner always emits depth-6 HEALPix output regardless.)
pub const GAIA_HEALPIX_LEVEL: u32 = 5;

/// Number of nested HEALPix level-5 pixels: `12 · 4^5`.
pub const GAIA_TILE_COUNT: u64 = 12_288;

/// `source_id` span per level-5 tile: `2^35 · 4^(12−5)` = `2^49`.
pub const SOURCE_ID_TILE_SPAN: u64 = 1 << 49;

/// Concurrent in-flight TAP jobs. ESA publishes no hard scripted limit but
/// asks users to be considerate; 3 is the documented fair-use ceiling and
/// gives a ~3× speedup over serial without abusing the service.
pub const GAIA_CONCURRENCY: usize = 3;

/// Progress callback for the query and conversion phases (mirrors
/// [`super::tycho2::Tycho2Progress`]).
pub enum GaiaProgress {
    /// Emitted immediately when the run begins (before the first slow TAP
    /// round-trip) so the UI has instant feedback. `already_done` is the
    /// resume point (tiles already in the manifest).
    Started {
        total_tiles: u64,
        already_done: u64,
    },
    /// One TAP tile finished. `completed` is the cumulative resume-aware
    /// count (monotonic — tiles finish out of order under concurrency, so
    /// drive progress bars off this, not `tile`).
    Querying {
        tile: u64,
        completed: u64,
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

/// Inclusive `source_id` range `[lo, hi]` covered by level-5 `tile`
/// (`0..GAIA_TILE_COUNT`). Tiles tile the `source_id` space contiguously.
pub fn tile_source_id_range(tile: u64) -> (u64, u64) {
    let lo = tile * SOURCE_ID_TILE_SPAN;
    let hi = (tile + 1) * SOURCE_ID_TILE_SPAN - 1;
    (lo, hi)
}

/// Build the ADQL for one level-5 tile: server-side magnitude cut + the
/// columns we need, constrained to the tile's `source_id` range (which is
/// the spatial partition — `source_id BETWEEN` is primary-key indexed, so
/// this is the fast form).
///
/// `ruwe` is projected so the raw CSV cache can be RUWE-filtered locally at
/// bin time. RUWE is **not** filtered server-side on purpose: it is a
/// per-star quality flag, not a size driver, so keeping it out of the
/// `WHERE` makes the cached download a full superset — the RUWE threshold
/// then becomes a local knob and changing it never needs a re-download.
pub fn tile_adql(tile: u64) -> String {
    let (lo, hi) = tile_source_id_range(tile);
    format!(
        "SELECT ra,dec,phot_g_mean_mag,pmra,pmdec,ruwe FROM gaiadr3.gaia_source \
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
        .connect_timeout(Duration::from_secs(30))
        // ESA closes idle keep-alive connections between our polls; reusing a
        // pooled-but-dead socket yields "connection closed before message
        // completed". Disable connection reuse — every request gets a fresh
        // connection. (Retries below cover the rest.)
        .pool_max_idle_per_host(0)
        .user_agent("athenaeum-catalog-ingest (astrophotography catalog builder)")
        .build()
        .context("build TAP HTTP client")
}

/// Retry a network op through transient failures (connection drops,
/// time-outs, 5xx) with exponential backoff. The TAP ops we wrap are all
/// safe to repeat (idempotent GETs; a re-submitted query just abandons a
/// duplicate job which ESA purges). A hard error (e.g. ADQL 400) simply
/// surfaces after the attempts with its real body intact.
fn with_retry<T>(what: &str, mut op: impl FnMut() -> Result<T>) -> Result<T> {
    const MAX: u32 = 6;
    let mut delay = Duration::from_secs(2);
    let mut last: Option<anyhow::Error> = None;
    for attempt in 1..=MAX {
        match op() {
            Ok(v) => return Ok(v),
            Err(e) => {
                eprintln!(
                    "gaia: {what} attempt {attempt}/{MAX} failed ({e:#}); \
                     retrying in {}s",
                    delay.as_secs()
                );
                last = Some(e);
                if attempt < MAX {
                    std::thread::sleep(delay);
                    delay = (delay * 2).min(Duration::from_secs(60));
                }
            }
        }
    }
    Err(last
        .unwrap()
        .context(format!("{what} failed after {MAX} attempts")))
}

/// Submit an async ADQL job (`PHASE=RUN`) and return the job resource URL.
/// ESA responds 303 → the job resource; reqwest follows it, so the final
/// response URL is the job URL.
pub fn submit_tap_job(client: &reqwest::blocking::Client, adql: &str) -> Result<String> {
    with_retry("TAP submit", || {
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
        let status = resp.status();
        let final_url = resp.url().as_str().trim_end_matches('/').to_string();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            anyhow::bail!(
                "TAP submit HTTP {status} (final url {final_url}): {}",
                body.chars().take(900).collect::<String>()
            );
        }
        if !final_url.contains("/async/") {
            let body = resp.text().unwrap_or_default();
            anyhow::bail!(
                "unexpected TAP job URL {final_url} (HTTP {status}): {}",
                body.chars().take(900).collect::<String>()
            );
        }
        Ok(final_url)
    })
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
        let phase = with_retry("poll TAP phase", || {
            client
                .get(format!("{job_url}/phase"))
                .send()
                .context("poll TAP phase")?
                .text()
                .context("read TAP phase")
        })?;
        match phase.trim() {
            "COMPLETED" => return Ok(()),
            "ERROR" | "ABORTED" => {
                // The UWS error document carries the real reason (ADQL
                // error, timeout, quota, …).
                let detail = client
                    .get(format!("{job_url}/error"))
                    .send()
                    .and_then(|r| r.text())
                    .unwrap_or_default();
                anyhow::bail!(
                    "TAP job {job_url} ended in {} — {}",
                    phase.trim(),
                    detail.chars().take(900).collect::<String>()
                );
            }
            "PENDING" | "QUEUED" | "EXECUTING" | "SUSPENDED" | "HELD" | "UNKNOWN" => {
                std::thread::sleep(POLL_INTERVAL)
            }
            // Anything else means the phase endpoint didn't return a UWS
            // phase (e.g. an HTML 404 → wrong job URL). Don't spin for 90
            // min on garbage — surface it.
            other => anyhow::bail!(
                "TAP {job_url}/phase returned non-UWS text: {}",
                other.chars().take(400).collect::<String>()
            ),
        }
    }
    anyhow::bail!("TAP job {job_url} did not complete within the poll cap")
}

/// Fetch the completed job's CSV result body.
pub fn fetch_job_csv(client: &reqwest::blocking::Client, job_url: &str) -> Result<String> {
    with_retry("fetch TAP result", || {
        let resp = client
            .get(format!("{job_url}/results/result"))
            .send()
            .context("fetch TAP result")?;
        let status = resp.status();
        let body = resp.text().context("read TAP result body")?;
        if !status.is_success() {
            anyhow::bail!(
                "TAP result HTTP {status}: {}",
                body.chars().take(900).collect::<String>()
            );
        }
        Ok(body)
    })
}

/// Parse one TAP CSV line into a [`StarRecord`]. Returns `None` for the
/// header, malformed/short rows, and rows that fail the RUWE quality cut.
///
/// Accepts both the 5-column legacy form (`ra,dec,phot_g_mean_mag,pmra,
/// pmdec`) and the 6-column form with a trailing `ruwe` — so old cached
/// CSVs and freshly-downloaded ones both parse.
///
/// Gaia 2-parameter astrometric solutions have empty `pmra`/`pmdec`; those
/// stars are still useful for matching, so a missing PM is treated as 0.
/// `ra`/`dec`/`phot_g_mean_mag` must be present (the server-side
/// `phot_g_mean_mag < 16` filter already excludes null-G rows). `pmra` is
/// Gaia μα\* (cos δ included) — stored as-is; this matches the Tycho-2 path
/// and `cone_search`'s proper-motion expectation (asserted in Task 7).
///
/// RUWE cut: a present 6th column parsed to a value >= [`GAIA_RUWE_MAX`]
/// drops the row. An absent or empty RUWE keeps the row (5-column input, or
/// a 2-parameter Gaia solution that has no RUWE by construction).
pub fn parse_gaia_csv_row(row: &str) -> Option<StarRecord> {
    let mut f = row.split(',');
    let ra: f64 = f.next()?.trim().parse().ok()?;
    let dec: f64 = f.next()?.trim().parse().ok()?;
    let g: f32 = f.next()?.trim().parse().ok()?;
    let pmra: f64 = match f.next()?.trim() {
        "" => 0.0,
        s => s.parse().ok()?,
    };
    let pmdec: f64 = match f.next()?.trim() {
        "" => 0.0,
        s => s.parse().ok()?,
    };
    // Optional 6th column: RUWE. Absent/empty → keep; present and bad → drop.
    if let Some(ruwe) = f.next().and_then(|s| {
        let t = s.trim();
        if t.is_empty() {
            None
        } else {
            t.parse::<f64>().ok()
        }
    }) {
        if ruwe >= GAIA_RUWE_MAX {
            return None;
        }
    }
    Some(StarRecord::from_values(ra as f32, dec as f32, g, pmra, pmdec))
}

/// RAM-safe streaming binner: the full G≤16 catalog (~300 M records ≈
/// 4.2 GB) cannot be held in memory like Tycho-2's 2.5 M, so we never
/// collect it. Each TAP tile (~390 k records ≈ 5.5 MB — bounded) is grouped
/// by depth-6 HEALPix pixel and **appended** to per-pixel scratch files
/// (`scratch/p_NNNNNN.raw`). [`HealpixBinner::finalize`] then makes one pass
/// per pixel: read it back, and write the final `healpix_NNNNNN.bin` via the
/// shared [`binary_format::write_records`] (same pixel assignment + same
/// mag-sort + same serialization as [`super::write_catalog_to_healpix`], so
/// the output is byte-identical — asserted in tests).
pub struct HealpixBinner {
    scratch_dir: PathBuf,
}

impl HealpixBinner {
    pub fn open(scratch_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(scratch_dir)
            .with_context(|| format!("create scratch dir {}", scratch_dir.display()))?;
        Ok(Self {
            scratch_dir: scratch_dir.to_path_buf(),
        })
    }

    /// Append one tile's records to their per-pixel scratch files. Only this
    /// tile is held in memory; scratch files are opened/closed per pixel
    /// (~64 depth-6 pixels per level-3 tile → cheap, no fd exhaustion).
    pub fn push_tile(&self, records: &[StarRecord]) -> Result<()> {
        let mut by_pixel: HashMap<u64, Vec<&StarRecord>> = HashMap::new();
        for r in records {
            let pixel = healpix::sky_to_pixel(r.ra as f64, r.dec as f64);
            by_pixel.entry(pixel).or_default().push(r);
        }
        for (pixel, recs) in by_pixel {
            let path = self.scratch_dir.join(format!("p_{:06}.raw", pixel));
            let f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .with_context(|| format!("append scratch {}", path.display()))?;
            let mut w = BufWriter::new(f);
            for r in recs {
                r.write_to(&mut w)?;
            }
            w.flush()?;
        }
        Ok(())
    }

    /// Read every pixel scratch file, mag-sort + serialize it into
    /// `out_dir/healpix_NNNNNN.bin` (via the shared writer), delete the
    /// scratch, and return the total record count.
    pub fn finalize(self, out_dir: &Path) -> Result<usize> {
        std::fs::create_dir_all(out_dir)
            .with_context(|| format!("create catalog dir {}", out_dir.display()))?;
        let mut total = 0usize;
        for entry in std::fs::read_dir(&self.scratch_dir)? {
            let path = entry?.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let Some(pixel_str) = name.strip_prefix("p_").and_then(|s| s.strip_suffix(".raw"))
            else {
                continue;
            };
            let pixel: u64 = pixel_str.parse().context("parse scratch pixel id")?;

            let mut records: Vec<StarRecord> = Vec::new();
            let f = std::fs::File::open(&path)?;
            let mut r = BufReader::new(f);
            while let Ok(rec) = StarRecord::read_from(&mut r) {
                records.push(rec);
            }
            total += records.len();

            let out = out_dir.join(format!("healpix_{:06}.bin", pixel));
            let mut w = BufWriter::new(std::fs::File::create(&out)?);
            binary_format::write_records(&mut w, &mut records)?;
            w.flush()?;
            std::fs::remove_file(&path)?;
        }
        Ok(total)
    }
}

/// Read the set of already-completed tile ids from `done.manifest`.
fn read_done_manifest(path: &Path) -> std::collections::HashSet<u64> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.lines().filter_map(|l| l.trim().parse::<u64>().ok()).collect())
        .unwrap_or_default()
}

/// Extract all 768 level-3 tiles via TAP into `scratch_dir/bins` (per-pixel
/// raw scratch), checkpointing each completed tile in
/// `scratch_dir/done.manifest`. Resumable: a re-run skips manifested tiles.
/// Returns the number of star records written this run (not cumulative
/// across resumes — `finalize` reports the true total).
pub fn download_gaia_dr3(
    scratch_dir: &Path,
    cancel_flag: Arc<AtomicBool>,
    progress: &dyn Fn(GaiaProgress),
) -> Result<usize> {
    std::fs::create_dir_all(scratch_dir)?;
    let manifest_path = scratch_dir.join("done.manifest");
    let done = read_done_manifest(&manifest_path);
    let binner = HealpixBinner::open(&scratch_dir.join("bins"))?;
    // Persistent raw-CSV cache: each tile's verbatim ESA CSV is kept here
    // forever. Nothing deletes it (`finalize` only removes the per-pixel
    // `bins/p_*.raw` scratch). The presence of `tile_NNN.csv` lets a re-run
    // skip the slow TAP round-trip — so a re-bin (new RUWE threshold, format
    // change) costs zero ESA traffic.
    let raw_csv_dir = scratch_dir.join("raw_csv");
    std::fs::create_dir_all(&raw_csv_dir)
        .with_context(|| format!("create raw-csv cache {}", raw_csv_dir.display()))?;
    let client = tap_client()?;

    // `GAIA_CONCURRENCY` producer threads each pull tiles from a shared
    // counter and do the slow network round-trip (submit → poll → fetch →
    // parse) in parallel; a single consumer (this thread) serializes the
    // disk side (binner + manifest) so there is no file contention and the
    // progress callback need not be `Send`. Bounded channel = backpressure
    // (≈one tile of stars per slot, so memory stays small).
    // Immediate feedback before the first (slow) TAP round-trip.
    let already_done = done.len() as u64;
    progress(GaiaProgress::Started {
        total_tiles: GAIA_TILE_COUNT,
        already_done,
    });

    let next = Arc::new(AtomicU64::new(0));
    let first_err: Arc<Mutex<Option<anyhow::Error>>> = Arc::new(Mutex::new(None));
    let (tx, rx) = mpsc::sync_channel::<(u64, Vec<StarRecord>)>(GAIA_CONCURRENCY);
    let mut written = 0usize;
    let mut completed = already_done;

    std::thread::scope(|s| {
        for _ in 0..GAIA_CONCURRENCY {
            let tx = tx.clone();
            let (next, first_err, cancel_flag) =
                (Arc::clone(&next), Arc::clone(&first_err), Arc::clone(&cancel_flag));
            let (client, done, raw_csv_dir) = (&client, &done, &raw_csv_dir);
            s.spawn(move || loop {
                let tile = next.fetch_add(1, Ordering::Relaxed);
                if tile >= GAIA_TILE_COUNT || cancel_flag.load(Ordering::Relaxed) {
                    break;
                }
                if done.contains(&tile) {
                    continue;
                }
                let fetch = (|| -> Result<Vec<StarRecord>> {
                    // Cached CSV present → re-bin from disk, no TAP round-trip.
                    let csv_path = raw_csv_dir.join(format!("tile_{tile:05}.csv"));
                    let csv = if csv_path.is_file() {
                        std::fs::read_to_string(&csv_path).with_context(|| {
                            format!("read cached tile CSV {}", csv_path.display())
                        })?
                    } else {
                        let job = submit_tap_job(client, &tile_adql(tile))?;
                        poll_job(client, &job, &cancel_flag)?;
                        let fetched = fetch_job_csv(client, &job)?;
                        // Persist the raw CSV before parsing, so future
                        // rebuilds never re-download. Temp-then-rename so a
                        // crash mid-write cannot leave a truncated file that
                        // looks complete.
                        let tmp = csv_path.with_extension("csv.partial");
                        std::fs::write(&tmp, &fetched).with_context(|| {
                            format!("write cached tile CSV {}", tmp.display())
                        })?;
                        std::fs::rename(&tmp, &csv_path).with_context(|| {
                            format!("commit cached tile CSV {}", csv_path.display())
                        })?;
                        fetched
                    };
                    Ok(csv.lines().filter_map(parse_gaia_csv_row).collect())
                })();
                match fetch {
                    Ok(recs) => {
                        if tx.send((tile, recs)).is_err() {
                            break; // consumer gone
                        }
                    }
                    Err(e) => {
                        let mut slot = first_err.lock().unwrap();
                        if slot.is_none() {
                            *slot = Some(e.context(format!("tile {tile}")));
                        }
                        cancel_flag.store(true, Ordering::Relaxed);
                        break;
                    }
                }
            });
        }
        // Drop our own sender so the channel closes once every producer ends.
        drop(tx);

        // Consumer: serialize disk writes + checkpoint + progress.
        while let Ok((tile, recs)) = rx.recv() {
            if let Err(e) = binner.push_tile(&recs) {
                let mut slot = first_err.lock().unwrap();
                if slot.is_none() {
                    *slot = Some(e.context(format!("binner push tile {tile}")));
                }
                cancel_flag.store(true, Ordering::Relaxed);
                continue;
            }
            let checkpoint = (|| -> Result<()> {
                let mut mf = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&manifest_path)
                    .context("open done.manifest")?;
                writeln!(mf, "{tile}")?;
                mf.flush()?;
                Ok(())
            })();
            if let Err(e) = checkpoint {
                let mut slot = first_err.lock().unwrap();
                if slot.is_none() {
                    *slot = Some(e.context(format!("checkpoint tile {tile}")));
                }
                cancel_flag.store(true, Ordering::Relaxed);
                continue;
            }
            written += recs.len();
            completed += 1;
            progress(GaiaProgress::Querying {
                tile,
                completed,
                total_tiles: GAIA_TILE_COUNT,
                stars: recs.len(),
            });
        }
    });

    if let Some(e) = first_err.lock().unwrap().take() {
        return Err(e);
    }
    Ok(written)
}

/// Full Gaia DR3 (G≤19) catalog setup. Idempotent (skips if
/// `catalogs/gaia_dr3/` already populated, mirroring
/// [`super::tycho2::setup_tycho2_catalog`]); resumable via the tile
/// manifest. Returns the catalog directory, auto-discovered by
/// [`crate::catalog::CatalogEngine::with_catalog_dir`].
pub fn setup_gaia_dr3_catalog(
    app_data_dir: &Path,
    cancel_flag: Arc<AtomicBool>,
    progress: &dyn Fn(GaiaProgress),
) -> Result<PathBuf> {
    let catalog_dir = app_data_dir.join("catalogs").join("gaia_dr3");
    if catalog_dir.exists() {
        let file_count = std::fs::read_dir(&catalog_dir)?.count();
        if file_count > 100 {
            eprintln!("gaia: catalog already exists with {file_count} files");
            return Ok(catalog_dir);
        }
    }

    let scratch_dir = app_data_dir.join("gaia_dr3_raw");
    let written = download_gaia_dr3(&scratch_dir, cancel_flag, progress)?;
    progress(GaiaProgress::Converting {
        stars_processed: 0,
        total_stars: written,
    });

    let binner = HealpixBinner::open(&scratch_dir.join("bins"))?;
    let total = binner.finalize(&catalog_dir)?;
    progress(GaiaProgress::Complete { total_stars: total });
    Ok(catalog_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_range_partitions_source_id_space() {
        // Tile 0 starts at 0.
        assert_eq!(tile_source_id_range(0).0, 0);
        // Span is exactly 2^49 (level-5 tiling).
        let (lo0, hi0) = tile_source_id_range(0);
        assert_eq!(hi0 - lo0 + 1, SOURCE_ID_TILE_SPAN);
        assert_eq!(SOURCE_ID_TILE_SPAN, 562_949_953_421_312);

        // Contiguous + non-overlapping across all tiles.
        for t in 0..GAIA_TILE_COUNT - 1 {
            let (_, hi) = tile_source_id_range(t);
            let (next_lo, _) = tile_source_id_range(t + 1);
            assert_eq!(hi + 1, next_lo, "gap/overlap between tile {t} and {}", t + 1);
        }

        // All tiles cover [0, GAIA_TILE_COUNT·SOURCE_ID_TILE_SPAN).
        let (last_lo, last_hi) = tile_source_id_range(GAIA_TILE_COUNT - 1);
        assert_eq!(last_lo, (GAIA_TILE_COUNT - 1) * SOURCE_ID_TILE_SPAN);
        assert_eq!(last_hi + 1, GAIA_TILE_COUNT * SOURCE_ID_TILE_SPAN);
        // …and stay within the u64 source_id space (12 288·2^49 = 12·2^59 < 2^63).
        assert!(GAIA_TILE_COUNT * SOURCE_ID_TILE_SPAN < (1u64 << 63));
    }

    #[test]
    fn adql_is_well_formed() {
        let q0 = tile_adql(0);
        assert!(q0.contains("FROM gaiadr3.gaia_source"));
        assert!(q0.contains("phot_g_mean_mag < 19"));
        // Tile 0 spans [0, 2^49 - 1].
        assert!(
            q0.contains("source_id BETWEEN 0 AND 562949953421311"),
            "tile 0 range wrong: {q0}"
        );
        // Tile 1's lower bound is exactly 2^49.
        let q1 = tile_adql(1);
        assert!(
            q1.contains("BETWEEN 562949953421312 AND "),
            "tile 1 lower bound wrong: {q1}"
        );
        // The 5 stored columns plus ruwe (for the local quality cut).
        assert!(q0.starts_with("SELECT ra,dec,phot_g_mean_mag,pmra,pmdec,ruwe FROM"));
        // RUWE is projected but NOT filtered server-side.
        assert!(!q0.contains("ruwe <") && !q0.contains("ruwe<"));
    }

    #[test]
    fn parse_row_and_header() {
        // Header → None.
        assert!(parse_gaia_csv_row("ra,dec,phot_g_mean_mag,pmra,pmdec").is_none());
        // Full row.
        let s = parse_gaia_csv_row("86.682,0.042,14.231,12.5,-7.25").expect("row");
        assert!((s.ra - 86.682).abs() < 1e-3);
        assert!((s.dec - 0.042).abs() < 1e-3);
        assert!((s.mag() - 14.231).abs() < 1e-3);
        assert!((s.pmra_mas_yr() - 12.5).abs() < 0.01);
        assert!((s.pmdec_mas_yr() - (-7.25)).abs() < 0.01);
        // Null PM (2-parameter solution) → PM 0, still Some.
        let s2 = parse_gaia_csv_row("10.0,20.0,15.9,,").expect("null-pm row");
        assert_eq!(s2.pmra_mas_yr(), 0.0);
        assert_eq!(s2.pmdec_mas_yr(), 0.0);
        assert!((s2.mag() - 15.9).abs() < 1e-3);
        // Short/garbage rows → None.
        assert!(parse_gaia_csv_row("1.0,2.0").is_none());
        assert!(parse_gaia_csv_row("").is_none());
        // 6-column form with a good RUWE → kept.
        let s3 = parse_gaia_csv_row("12.0,34.0,13.0,1.1,2.2,1.05").expect("good-ruwe row");
        assert!((s3.mag() - 13.0).abs() < 1e-3);
        // 6-column form with RUWE >= 1.4 → dropped (unreliable astrometry).
        assert!(parse_gaia_csv_row("12.0,34.0,13.0,1.1,2.2,1.40").is_none());
        assert!(parse_gaia_csv_row("12.0,34.0,13.0,1.1,2.2,3.7").is_none());
        // 6-column form with empty RUWE (2-param solution) → kept.
        assert!(parse_gaia_csv_row("12.0,34.0,13.0,,,").is_some());
    }

    #[test]
    fn binner_roundtrips_and_sorts_byte_identical_to_write_catalog() {
        use crate::catalog::write_catalog_to_healpix;
        use tempfile::TempDir;

        // ~10 stars spanning several depth-6 pixels, magnitudes deliberately
        // out of order so the per-pixel mag sort is exercised.
        let recs: Vec<StarRecord> = vec![
            StarRecord::from_values(10.0, 10.0, 12.5, 1.0, -2.0),
            StarRecord::from_values(10.05, 10.02, 8.1, 0.0, 0.0),
            StarRecord::from_values(10.02, 9.98, 15.9, -3.0, 4.0),
            StarRecord::from_values(200.0, -40.0, 9.7, 5.0, 5.0),
            StarRecord::from_values(200.1, -40.1, 6.3, 0.0, 0.0),
            StarRecord::from_values(200.2, -39.9, 14.2, 2.0, -1.0),
            StarRecord::from_values(300.0, 70.0, 11.0, 0.0, 0.0),
            StarRecord::from_values(300.3, 70.2, 7.4, -1.0, 1.0),
            StarRecord::from_values(45.0, -5.0, 13.3, 0.0, 0.0),
            StarRecord::from_values(45.0, -5.0, 10.0, 0.0, 0.0),
        ];

        let scratch = TempDir::new().unwrap();
        let out_binner = TempDir::new().unwrap();
        let binner = HealpixBinner::open(scratch.path()).unwrap();
        // Two push_tile calls → exercises cross-tile append into the same
        // pixel scratch file.
        binner.push_tile(&recs[..5]).unwrap();
        binner.push_tile(&recs[5..]).unwrap();
        let total = binner.finalize(out_binner.path()).unwrap();
        assert_eq!(total, recs.len());

        let out_ref = TempDir::new().unwrap();
        write_catalog_to_healpix(&recs, out_ref.path()).unwrap();

        // Same set of pixel files, each byte-identical.
        let names = |d: &Path| {
            let mut v: Vec<String> = std::fs::read_dir(d)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            v.sort();
            v
        };
        let bn = names(out_binner.path());
        let rn = names(out_ref.path());
        assert!(!bn.is_empty() && bn.len() >= 3, "expected multiple pixels: {bn:?}");
        assert_eq!(bn, rn, "pixel file sets differ");
        for n in &bn {
            let a = std::fs::read(out_binner.path().join(n)).unwrap();
            let b = std::fs::read(out_ref.path().join(n)).unwrap();
            assert_eq!(a, b, "pixel file {n} not byte-identical to write_catalog_to_healpix");
        }
        // Scratch fully consumed.
        assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
    }

    #[test]
    fn setup_is_idempotent_when_catalog_present() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let cat = tmp.path().join("catalogs").join("gaia_dr3");
        std::fs::create_dir_all(&cat).unwrap();
        // >100 files → setup must return immediately, no network touched.
        for i in 0..101 {
            std::fs::write(cat.join(format!("healpix_{i:06}.bin")), b"x").unwrap();
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let got = setup_gaia_dr3_catalog(tmp.path(), cancel, &|_| {}).unwrap();
        assert_eq!(got, cat);
    }

    #[test]
    fn done_manifest_parsing_round_trips() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let mf = tmp.path().join("done.manifest");
        std::fs::write(&mf, "0\n5\n767\n\n  12 \n").unwrap();
        let set = read_done_manifest(&mf);
        assert!(set.contains(&0) && set.contains(&5) && set.contains(&767) && set.contains(&12));
        assert_eq!(set.len(), 4);
        // Missing file → empty set (fresh run).
        assert!(read_done_manifest(&tmp.path().join("nope")).is_empty());
    }

    #[test]
    fn gaia_pm_convention_matches_tycho2_path() {
        use astroimage::platesolving::ProperMotionCorrector;

        // parse_gaia_csv_row must store Gaia μα* (cos δ included) verbatim in
        // mas/yr — the SAME representation the Tycho-2 path and cone_search's
        // ProperMotionCorrector expect. Realistic PM (StarRecord stores pmra
        // as i16 ×0.01 mas/yr → ±327.67 clamp; only a handful of very-high-PM
        // stars clamp, irrelevant for plate solving).
        let s = parse_gaia_csv_row("0.0,60.0,10.0,300.0,-50.0").expect("row");
        assert!(
            (s.pmra_mas_yr() - 300.0).abs() < 0.01 && (s.pmdec_mas_yr() + 50.0).abs() < 0.01,
            "Gaia pmra/pmdec must be stored as-is in mas/yr (no cos δ / scale applied at parse), got ({}, {})",
            s.pmra_mas_yr(),
            s.pmdec_mas_yr()
        );

        // Assumption-free guarantee: a Gaia-parsed record fed to the shared
        // corrector is bit-identical to the canonical StarRecord path with
        // the same numbers — so the Gaia route introduces zero PM distortion
        // / cos δ double-count relative to the Tycho-2 route.
        let direct = StarRecord::from_values(0.0, 60.0, 10.0, 300.0, -50.0);
        let g = ProperMotionCorrector::propagate(
            s.ra as f64, s.dec as f64, s.pmra_mas_yr(), s.pmdec_mas_yr(), 2016.0, 2116.0,
        );
        let d = ProperMotionCorrector::propagate(
            direct.ra as f64, direct.dec as f64, direct.pmra_mas_yr(), direct.pmdec_mas_yr(), 2016.0, 2116.0,
        );
        assert_eq!(g, d, "Gaia path must match the canonical StarRecord path exactly");
        // Sanity: nonzero pmra moves RA over 100 yr.
        assert!((g.0 - 0.0).abs() > 1e-9, "nonzero pmra must shift RA");
    }

    #[test]
    fn tile_count_matches_healpix_level() {
        // Nested HEALPix: 12 · 4^level. Level 5 → 12 288 tiles.
        assert_eq!(12 * 4u64.pow(GAIA_HEALPIX_LEVEL), GAIA_TILE_COUNT);
        assert_eq!(GAIA_TILE_COUNT, 12_288);
    }
}

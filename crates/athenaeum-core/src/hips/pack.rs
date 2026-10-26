//! Offline pack selection (issue #18, phase 2): turns "whole sky to order k
//! plus detail around chosen regions" into the deduplicated list of tiles to
//! download (pure computation, no I/O), and runs that list into the
//! [`HipsStore`] on worker threads.
//!
//! The renderer falls back to *ancestors only*, so a detail region covers
//! every order from `k + 1` up to [`super::DISPLAY_MAX_ORDER`]: a gap between
//! k and the deepest order would show the coarse sky.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use super::{FetchedTile, HipsStore, ServerBusy, TileFormat, DISPLAY_MAX_ORDER};
use crate::models::ImagingLocation;
use crate::plate_solve::dso_lookup::DsoCatalog;

/// Deepest whole-sky order a pack may use. Order 6 for the whole sky is
/// ~49 000 tiles and is never a pack.
pub const MAX_SKY_ORDER: u8 = 5;
/// Radius around a catalog object, degrees.
const CATALOG_RADIUS_DEG: f64 = 3.0;
/// Margin added to half the field diagonal of an imaging location, degrees.
const LOCATION_MARGIN_DEG: f64 = 1.0;
/// Radius of an imaging location that has no field of view, degrees.
const LOCATION_NO_FOV_RADIUS_DEG: f64 = 2.0;

/// What the user ticked on the Settings page.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PackSelection {
    pub survey_id: String,
    /// Whole-sky order, 0..=5.
    pub sky_order: u8,
    pub around_locations: bool,
    pub around_messier: bool,
    pub around_ngc_ic: bool,
}

impl Default for PackSelection {
    fn default() -> Self {
        Self {
            survey_id: "dss-color".to_string(),
            sky_order: 4,
            around_locations: true,
            around_messier: true,
            around_ngc_ic: false,
        }
    }
}

/// A cone on the sky that gets full detail.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PackRegion {
    pub ra_deg: f64,
    pub dec_deg: f64,
    pub radius_deg: f64,
}

/// Validate a whole-sky order (0..=5).
pub fn checked_sky_order(order: u8) -> Result<u8> {
    if order > MAX_SKY_ORDER {
        bail!("whole-sky pack order must be 0–5");
    }
    Ok(order)
}

/// One region per imaging location (frame set or cluster): half the diagonal
/// of its field of view plus 1 degree, or 2 degrees when it has no field of
/// view. Locations without a finite position are skipped.
pub fn location_regions(locs: &[ImagingLocation]) -> Vec<PackRegion> {
    locs.iter()
        .filter(|l| l.ra.is_finite() && l.dec.is_finite())
        .map(|l| {
            let radius_deg = match (l.fov_width, l.fov_height) {
                (Some(w), Some(h)) if w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0 => {
                    0.5 * w.hypot(h) + LOCATION_MARGIN_DEG
                }
                _ => LOCATION_NO_FOV_RADIUS_DEG,
            };
            PackRegion {
                ra_deg: l.ra,
                dec_deg: l.dec,
                radius_deg,
            }
        })
        .collect()
}

/// One 3 degree region per catalog object of the ticked groups. Messier means
/// a designation starting with `"M "`; NGC/IC means the first word is `NGC` or
/// `IC` and the magnitude is 10 or brighter.
pub fn catalog_regions(cat: &DsoCatalog, messier: bool, ngc_ic: bool) -> Vec<PackRegion> {
    cat.entries()
        .iter()
        .filter(|d| {
            let first = d.designation.split_whitespace().next().unwrap_or("");
            let is_messier = d.designation.starts_with("M ");
            let is_ngc_ic =
                matches!(first, "NGC" | "IC") && d.magnitude.is_some_and(|m| m <= 10.0);
            (messier && is_messier) || (ngc_ic && is_ngc_ic)
        })
        .map(|d| PackRegion {
            ra_deg: d.ra_deg,
            dec_deg: d.dec_deg,
            radius_deg: CATALOG_RADIUS_DEG,
        })
        .collect()
}

/// The tiles of a pack as `(order, npix)`, sorted ascending and deduplicated:
/// every tile of orders `0..=sky_order`, then for each region the cone
/// coverage at orders `sky_order + 1..=6`.
pub fn pack_tiles(sky_order: u8, regions: &[PackRegion]) -> Result<Vec<(u8, u64)>> {
    let sky_order = checked_sky_order(sky_order)?;
    let mut tiles: Vec<(u8, u64)> = Vec::new();
    for order in 0..=sky_order {
        let n = 12u64 << (2 * u32::from(order));
        tiles.extend((0..n).map(|npix| (order, npix)));
    }
    for region in regions {
        for order in sky_order + 1..=DISPLAY_MAX_ORDER {
            tiles.extend(
                crate::catalog::healpix::cone_pixels_at(
                    order,
                    region.ra_deg.rem_euclid(360.0),
                    region.dec_deg.clamp(-90.0, 90.0),
                    region.radius_deg,
                )
                .into_iter()
                .map(|npix| (order, npix)),
            );
        }
    }
    tiles.sort_unstable();
    tiles.dedup();
    Ok(tiles)
}

// ---------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------

/// Where a pack stands. Serialised lowercase.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
pub enum PackState {
    #[default]
    Idle,
    Running,
    Cancelling,
    Done,
    Cancelled,
    Failed,
    /// The app quit or crashed while a pack ran; Resume continues it.
    Interrupted,
}

impl PackState {
    /// The serialised word, for log fields.
    pub fn as_str(self) -> &'static str {
        match self {
            PackState::Idle => "idle",
            PackState::Running => "running",
            PackState::Cancelling => "cancelling",
            PackState::Done => "done",
            PackState::Cancelled => "cancelled",
            PackState::Failed => "failed",
            PackState::Interrupted => "interrupted",
        }
    }
}

/// A snapshot of a pack run. Done when `present + fetched + absent + failed
/// == total`.
///
/// Also what `<store>/pack.json` holds. Every field defaults, so a file an
/// older build wrote (selection and state only) still parses.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", default)]
pub struct PackProgress {
    pub state: PackState,
    pub selection: Option<PackSelection>,
    pub total: u64,
    /// Tiles already on disk when the run started.
    pub present: u64,
    pub fetched: u64,
    /// Tiles the master answered 404 for (no coverage).
    pub absent: u64,
    pub failed: u64,
    pub bytes_fetched: u64,
    /// Seconds the survey asked us to wait, while at least one worker waits.
    pub waiting_s: Option<u32>,
    pub error: Option<String>,
}

impl PackProgress {
    /// The `initial` of [`run_pack`]: a running pack of `total` tiles.
    pub fn new(selection: PackSelection, total: u64) -> Self {
        PackProgress { state: PackState::Running, selection: Some(selection), total, ..Default::default() }
    }
}

/// Runner tuning. Tests pass millisecond durations.
#[derive(Debug, Clone)]
pub struct PackRunConfig {
    pub workers: usize,
    /// Longest wait for a busy survey, whatever `Retry-After` says.
    pub busy_cap: Duration,
    /// First wait when the survey sent no `Retry-After`; doubles per wait.
    pub backoff_start: Duration,
    /// A tile still busy after this many waits counts as failed.
    pub max_busy_waits: u32,
    /// This many request errors in a row stop the run as `Failed`.
    pub max_consecutive_errors: u32,
    /// This many tiles in a row that exhausted `max_busy_waits` stop the run
    /// as `Failed` ("survey busy"). A fetched tile or a 404 resets the count;
    /// busy tiles never count toward `max_consecutive_errors`.
    pub max_consecutive_busy_failures: u32,
}

impl Default for PackRunConfig {
    fn default() -> Self {
        PackRunConfig {
            workers: 4,
            busy_cap: Duration::from_secs(300),
            backoff_start: Duration::from_secs(5),
            max_busy_waits: 6,
            max_consecutive_errors: 20,
            max_consecutive_busy_failures: 4,
        }
    }
}

/// Progress callbacks are throttled to this interval (4 per second).
const EMIT_INTERVAL: Duration = Duration::from_millis(250);
/// A wait checks the cancel flag at least this often.
const SLEEP_STEP: Duration = Duration::from_millis(200);

/// What the store says when no run is live in this process: the last snapshot
/// in `pack.json` (counts included), `Interrupted` if it still says `Running`
/// (the app died mid-run), else `Idle`. Reads the file once; an unreadable
/// file is logged and counts as `Idle`, never an error that sticks.
pub fn stored_status(store: &HipsStore) -> PackProgress {
    match store.read_pack_state() {
        Ok(Some(mut p)) => {
            if matches!(p.state, PackState::Running | PackState::Cancelling) {
                p.state = PackState::Interrupted;
            }
            p.waiting_s = None;
            p
        }
        Ok(None) => PackProgress::default(),
        Err(e) => {
            tracing::warn!(path = %store.root().display(), error = %format!("{e:#}"), "hips pack state unreadable");
            PackProgress::default()
        }
    }
}

/// How long to wait before retrying a busy tile. The cap is applied to the
/// server's value before any arithmetic: `Retry-After: u64::MAX` is a valid
/// `Duration`. The server's value is floored at `backoff_start`, so
/// `Retry-After: 0` never turns into a request loop.
fn busy_wait(retry_after: Option<Duration>, waits_so_far: u32, cfg: &PackRunConfig) -> Duration {
    match retry_after {
        Some(d) => d.min(cfg.busy_cap).max(cfg.backoff_start),
        None => cfg
            .backoff_start
            .checked_mul(1u32 << waits_so_far.min(20))
            .map_or(cfg.busy_cap, |d| d.min(cfg.busy_cap)),
    }
}

enum Outcome {
    Image(u64),
    Absent,
    /// Still busy after the allowed waits. Not a network error.
    Busy(anyhow::Error),
    /// A request or store error; counts toward the consecutive-error stop.
    Error(anyhow::Error),
    /// Cancel or stop arrived while waiting; the tile is left for a resume.
    Halted,
}

struct Shared {
    progress: PackProgress,
    consecutive_errors: u32,
    /// Tiles in a row that stayed busy through all their waits.
    consecutive_busy_failures: u32,
    last_emit: Instant,
    /// A change the throttle held back; the next flush emits it.
    dirty: bool,
    waiting_workers: u32,
    wait_s: u32,
}

struct Run<'a> {
    store: &'a HipsStore,
    base_url: &'a str,
    format: TileFormat,
    todo: &'a [(u8, u64)],
    cfg: &'a PackRunConfig,
    cancel: &'a AtomicBool,
    on_progress: &'a (dyn Fn(&PackProgress) + Sync),
    survey_id: String,
    next: AtomicUsize,
    /// Set by the run itself when it gives up (too many errors).
    stop: AtomicBool,
    shared: Mutex<Shared>,
}

impl Run<'_> {
    fn halted(&self) -> bool {
        self.cancel.load(Ordering::Relaxed) || self.stop.load(Ordering::Relaxed)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Mutates the shared progress and emits it when the throttle allows.
    /// `on_progress` runs under the lock, so calls are ordered; it must not
    /// call back into the run.
    fn update(&self, f: impl FnOnce(&mut Shared)) {
        let mut s = self.lock();
        f(&mut s);
        s.progress.waiting_s = (s.waiting_workers > 0).then_some(s.wait_s);
        if s.last_emit.elapsed() >= EMIT_INTERVAL {
            s.last_emit = Instant::now();
            s.dirty = false;
            (self.on_progress)(&s.progress);
        } else {
            s.dirty = true;
        }
    }

    /// Emits a change the throttle held back, once the interval has passed.
    /// Returns true when nothing is left to emit.
    fn flush_if_due(&self) -> bool {
        let mut s = self.lock();
        if !s.dirty {
            return true;
        }
        if s.last_emit.elapsed() < EMIT_INTERVAL {
            return false;
        }
        s.last_emit = Instant::now();
        s.dirty = false;
        (self.on_progress)(&s.progress);
        true
    }

    /// Waits (at most one throttle interval) until the held-back change is
    /// emitted, so the end of a busy wait reaches the UI too.
    fn settle(&self) {
        while !self.halted() && !self.flush_if_due() {
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Sleeps in steps, watching the flags. True when it was cut short.
    fn sleep(&self, total: Duration) -> bool {
        let deadline = Instant::now().checked_add(total).unwrap_or_else(|| Instant::now() + self.cfg.busy_cap);
        loop {
            if self.halted() {
                return true;
            }
            self.flush_if_due();
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            std::thread::sleep(left.min(SLEEP_STEP));
        }
    }

    fn process(&self, order: u8, npix: u64) -> Outcome {
        let mut waits = 0u32;
        let fetched = loop {
            match super::fetch_tile_from(self.base_url, self.format, order, npix) {
                Ok(t) => break t,
                Err(e) => {
                    let Some(busy) = e.downcast_ref::<ServerBusy>().copied() else {
                        return Outcome::Error(e);
                    };
                    if waits >= self.cfg.max_busy_waits {
                        return Outcome::Busy(e);
                    }
                    let wait = busy_wait(busy.retry_after, waits, self.cfg);
                    waits += 1;
                    tracing::debug!(
                        survey_id = %self.survey_id,
                        hips_order = order,
                        npix,
                        retry_after_s = wait.as_secs(),
                        "hips survey busy"
                    );
                    self.update(|s| {
                        s.waiting_workers += 1;
                        s.wait_s = u32::try_from(wait.as_millis().div_ceil(1000)).unwrap_or(u32::MAX);
                    });
                    let cut = self.sleep(wait);
                    self.update(|s| s.waiting_workers -= 1);
                    if cut {
                        return Outcome::Halted;
                    }
                    if wait >= EMIT_INTERVAL {
                        // A shorter wait is invisible to the UI anyway.
                        self.settle();
                    }
                }
            }
        };
        match fetched {
            FetchedTile::Image(bytes) => match self.store.write(order, npix, &bytes) {
                Ok(()) => Outcome::Image(bytes.len() as u64),
                Err(e) => Outcome::Error(e),
            },
            FetchedTile::Absent => match self.store.mark_absent(order, npix) {
                Ok(()) => Outcome::Absent,
                Err(e) => Outcome::Error(e),
            },
        }
    }

    fn worker(&self) {
        while !self.halted() {
            let i = self.next.fetch_add(1, Ordering::Relaxed);
            let Some(&(order, npix)) = self.todo.get(i) else { return };
            match self.process(order, npix) {
                Outcome::Image(n) => self.update(|s| {
                    s.progress.fetched += 1;
                    s.progress.bytes_fetched += n;
                    s.consecutive_errors = 0;
                    s.consecutive_busy_failures = 0;
                }),
                Outcome::Absent => self.update(|s| {
                    s.progress.absent += 1;
                    s.consecutive_errors = 0;
                    s.consecutive_busy_failures = 0;
                }),
                Outcome::Busy(e) => {
                    self.warn_failed(order, npix, &e);
                    let msg = format!("{e:#}");
                    self.update(|s| {
                        s.progress.failed += 1;
                        s.consecutive_busy_failures += 1;
                        if s.consecutive_busy_failures >= self.cfg.max_consecutive_busy_failures {
                            s.progress.error = Some(format!("survey busy: {msg}"));
                            self.stop.store(true, Ordering::Relaxed);
                        }
                    });
                }
                Outcome::Error(e) => {
                    self.warn_failed(order, npix, &e);
                    let msg = format!("{e:#}");
                    self.update(|s| {
                        s.progress.failed += 1;
                        s.consecutive_errors += 1;
                        if s.consecutive_errors >= self.cfg.max_consecutive_errors {
                            s.progress.error =
                                Some(format!("{} requests failed in a row; last: {msg}", s.consecutive_errors));
                            self.stop.store(true, Ordering::Relaxed);
                        }
                    });
                }
                Outcome::Halted => return,
            }
        }
    }

    fn warn_failed(&self, order: u8, npix: u64, e: &anyhow::Error) {
        tracing::warn!(
            survey_id = %self.survey_id,
            hips_order = order,
            npix,
            error = %format!("{e:#}"),
            "hips pack tile failed"
        );
    }
}

/// Downloads every tile of `tiles` that the store lacks. Synchronous: the
/// caller owns the thread. `initial` carries the selection (and state
/// `Running`); `total` is taken from `tiles`.
///
/// Tiles already stored count as `present` without any request. A `429`/`503`
/// makes the tile wait and retry (see [`PackRunConfig`]); a 404 is recorded as
/// absent. Cancel finishes the requests in flight and ends `Cancelled`;
/// `max_consecutive_errors` request errors in a row, or
/// `max_consecutive_busy_failures` busy tiles in a row, end `Failed`. Finished
/// tiles are kept either way, so a later run only fetches the rest.
/// `<store>/pack.json` holds `running` while the run lives and the end state
/// afterwards.
pub fn run_pack(
    store: &HipsStore,
    base_url: &str,
    format: TileFormat,
    tiles: &[(u8, u64)],
    initial: PackProgress,
    cfg: &PackRunConfig,
    cancel: &AtomicBool,
    on_progress: &(dyn Fn(&PackProgress) + Sync),
) -> PackProgress {
    // Once per run, before any worker writes: part files a quit left behind.
    store.sweep_stale_parts();
    let todo: Vec<(u8, u64)> = tiles.iter().copied().filter(|&(o, n)| !store.has(o, n)).collect();
    let selection = initial.selection.clone();
    let survey_id = selection.as_ref().map(|s| s.survey_id.clone()).unwrap_or_default();
    let mut progress = initial;
    progress.state = PackState::Running;
    progress.total = tiles.len() as u64;
    progress.present = (tiles.len() - todo.len()) as u64;

    tracing::info!(survey_id = %survey_id, tiles_total = progress.total, "hips pack started");
    write_pack_file(store, &progress);
    (on_progress)(&progress);

    let run = Run {
        store,
        base_url,
        format,
        todo: &todo,
        cfg,
        cancel,
        on_progress,
        survey_id,
        next: AtomicUsize::new(0),
        stop: AtomicBool::new(false),
        shared: Mutex::new(Shared {
            progress,
            consecutive_errors: 0,
            consecutive_busy_failures: 0,
            last_emit: Instant::now(),
            dirty: false,
            waiting_workers: 0,
            wait_s: 0,
        }),
    };
    let workers = cfg.workers.clamp(1, todo.len().max(1));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| run.worker());
        }
    });

    let mut s = run.lock();
    let p = &mut s.progress;
    p.waiting_s = None;
    let unfinished = p.present + p.fetched + p.absent + p.failed < p.total;
    p.state = if run.stop.load(Ordering::Relaxed) {
        PackState::Failed
    } else if unfinished && cancel.load(Ordering::Relaxed) {
        PackState::Cancelled
    } else {
        PackState::Done
    };
    tracing::info!(
        survey_id = %run.survey_id,
        pack_state = p.state.as_str(),
        tiles_fetched = p.fetched,
        tiles_absent = p.absent,
        tiles_failed = p.failed,
        bytes = p.bytes_fetched,
        "hips pack finished"
    );
    write_pack_file(store, p);
    (on_progress)(p);
    p.clone()
}

fn write_pack_file(store: &HipsStore, progress: &PackProgress) {
    if let Err(e) = store.write_pack_state(progress) {
        tracing::warn!(pack_state = progress.state.as_str(), error = %format!("{e:#}"), "hips pack state not saved");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m31() -> PackRegion {
        PackRegion {
            ra_deg: 10.6847,
            dec_deg: 41.2690,
            radius_deg: 3.0,
        }
    }

    fn location(fov: Option<(f64, f64)>) -> ImagingLocation {
        ImagingLocation {
            id: 1,
            ra: 10.0,
            dec: 20.0,
            object_name: None,
            frame_count: 1,
            total_exposure: 1.0,
            filters: vec![],
            date_range: (String::new(), String::new()),
            frame_set_id: None,
            fov_width: fov.map(|f| f.0),
            fov_height: fov.map(|f| f.1),
            location_type: "frameset".to_string(),
            cameras: None,
            focal_lengths: None,
            is_custom: false,
            rotation: None,
        }
    }

    #[test]
    fn whole_sky_counts() {
        assert_eq!(pack_tiles(3, &[]).unwrap().len(), 1020);
        assert_eq!(pack_tiles(4, &[]).unwrap().len(), 4092);
    }

    #[test]
    fn whole_sky_order_6_refused() {
        assert!(checked_sky_order(6).is_err());
        assert!(pack_tiles(6, &[]).is_err());
        assert_eq!(checked_sky_order(5).unwrap(), 5);
        assert_eq!(checked_sky_order(0).unwrap(), 0);
    }

    #[test]
    fn region_adds_orders_above_sky_only() {
        let base = pack_tiles(4, &[]).unwrap();
        let with = pack_tiles(4, &[m31()]).unwrap();
        let added: Vec<_> = with.iter().filter(|t| !base.contains(t)).collect();
        assert!(added.iter().all(|(o, _)| *o == 5 || *o == 6), "{added:?}");
        // The whole-sky part is untouched.
        assert!(base.iter().all(|t| with.contains(t)));
        let o6 = added.iter().filter(|(o, _)| *o == 6).count();
        assert!((30..=90).contains(&o6), "order-6 tiles: {o6}");
        // Every order between k and 6 is present, or the renderer would
        // fall back to the coarse sky.
        assert!(added.iter().any(|(o, _)| *o == 5));
    }

    #[test]
    fn overlapping_regions_dedup() {
        assert_eq!(
            pack_tiles(4, &[m31(), m31()]).unwrap(),
            pack_tiles(4, &[m31()]).unwrap()
        );
    }

    #[test]
    fn no_regions_is_whole_sky_only() {
        assert_eq!(
            pack_tiles(4, &location_regions(&[])).unwrap(),
            pack_tiles(4, &[]).unwrap()
        );
    }

    #[test]
    fn location_radius_from_fov() {
        let r = location_regions(&[location(Some((2.0, 1.5))), location(None)]);
        assert_eq!(r.len(), 2);
        assert!((r[0].radius_deg - 2.25).abs() < 1e-12);
        assert_eq!(r[1].radius_deg, 2.0);
    }

    #[test]
    fn catalog_groups_counts() {
        let cat = DsoCatalog::load().unwrap();
        assert_eq!(catalog_regions(&cat, true, false).len(), 108);
        assert_eq!(catalog_regions(&cat, false, true).len(), 491);
        assert_eq!(catalog_regions(&cat, true, true).len(), 108 + 491);
        assert!(catalog_regions(&cat, false, false).is_empty());
    }

    #[test]
    fn degenerate_region_adds_nothing_and_does_not_panic() {
        let bad = PackRegion {
            ra_deg: f64::NAN,
            dec_deg: 0.0,
            radius_deg: 3.0,
        };
        assert_eq!(pack_tiles(4, &[bad]).unwrap(), pack_tiles(4, &[]).unwrap());
    }

    /// The base-cell boundary at +/-41.81 degrees must not take the pack down.
    #[test]
    fn knife_edge_cones_do_not_panic() {
        for dec in [41.60, 41.71, 41.81, 41.90, -41.71] {
            let r = PackRegion {
                ra_deg: 10.68,
                dec_deg: dec,
                radius_deg: 3.0,
            };
            let t = pack_tiles(4, &[r]).unwrap();
            assert!(t.len() > pack_tiles(4, &[]).unwrap().len(), "dec {dec}");
        }
    }

    // ---- runner -----------------------------------------------------------

    use crate::hips::DSS_COLOR;
    use std::collections::{HashMap, VecDeque};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;

    const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3];

    #[derive(Clone)]
    struct Resp {
        status: u16,
        headers: &'static str,
        body: Vec<u8>,
    }

    fn ok() -> Resp {
        Resp { status: 200, headers: "", body: JPEG.to_vec() }
    }

    fn status(status: u16, headers: &'static str) -> Resp {
        Resp { status, headers, body: Vec::new() }
    }

    /// Status 0: the server reads the request and closes without answering.
    fn hang_up() -> Resp {
        status(0, "")
    }

    fn url_path(order: u8, npix: u64) -> String {
        format!("/{}", crate::hips::tile_rel_path(order, npix, TileFormat::Jpeg).to_string_lossy())
    }

    /// A local HTTP server with a queue of responses per path. A queue pops
    /// until one response is left, which then repeats; an unknown path gets
    /// `default`. `on_response(n)` runs before the n-th response (1-based) is
    /// written.
    struct TestServer {
        base: String,
        addr: std::net::SocketAddr,
        stop: Arc<AtomicBool>,
        hits: Arc<Mutex<Vec<String>>>,
    }

    impl TestServer {
        fn start(
            routes: Vec<(String, Vec<Resp>)>,
            default: Resp,
            on_response: impl Fn(usize) + Send + Sync + 'static,
        ) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let hits = Arc::new(Mutex::new(Vec::new()));
            let queues: Arc<Mutex<HashMap<String, VecDeque<Resp>>>> =
                Arc::new(Mutex::new(routes.into_iter().map(|(p, q)| (p, q.into())).collect()));
            let counter = Arc::new(AtomicUsize::new(0));
            let on_response = Arc::new(on_response);
            let (stop2, hits2) = (stop.clone(), hits.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop2.load(Ordering::SeqCst) {
                        return;
                    }
                    let Ok(mut stream) = stream else { continue };
                    let (queues, hits, counter, on_response, default) =
                        (queues.clone(), hits2.clone(), counter.clone(), on_response.clone(), default.clone());
                    std::thread::spawn(move || {
                        let mut buf = [0u8; 4096];
                        let n = stream.read(&mut buf).unwrap_or(0);
                        let req = String::from_utf8_lossy(&buf[..n]);
                        let path = req.split_whitespace().nth(1).unwrap_or("").to_string();
                        hits.lock().unwrap().push(path.clone());
                        if default.status == 0 {
                            return; // hang up: drop the stream unanswered
                        }
                        let resp = match queues.lock().unwrap().get_mut(&path) {
                            Some(q) if q.len() > 1 => q.pop_front().unwrap(),
                            Some(q) => q.front().cloned().unwrap_or(default),
                            None => default,
                        };
                        on_response(counter.fetch_add(1, Ordering::SeqCst) + 1);
                        let head = format!(
                            "HTTP/1.1 {} X\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n",
                            resp.status,
                            resp.headers,
                            resp.body.len()
                        );
                        let _ = stream.write_all(head.as_bytes());
                        let _ = stream.write_all(&resp.body);
                    });
                }
            });
            TestServer { base: format!("http://{addr}"), addr, stop, hits }
        }

        /// Answers every tile with a JPEG.
        fn all_ok() -> Self {
            Self::start(Vec::new(), ok(), |_| {})
        }

        fn hits_for(&self, path: &str) -> usize {
            self.hits.lock().unwrap().iter().filter(|p| p.as_str() == path).count()
        }

        fn total_hits(&self) -> usize {
            self.hits.lock().unwrap().len()
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            let _ = TcpStream::connect(self.addr);
        }
    }

    fn quick_cfg() -> PackRunConfig {
        PackRunConfig {
            workers: 4,
            busy_cap: Duration::from_millis(50),
            backoff_start: Duration::from_millis(1),
            max_busy_waits: 6,
            max_consecutive_errors: 20,
            max_consecutive_busy_failures: 4,
        }
    }

    fn tiles_of(n: u64) -> Vec<(u8, u64)> {
        (0..n).map(|i| (3u8, i)).collect()
    }

    fn run(
        store: &HipsStore,
        base: &str,
        tiles: &[(u8, u64)],
        cfg: &PackRunConfig,
        cancel: &AtomicBool,
    ) -> PackProgress {
        let initial = PackProgress::new(PackSelection::default(), tiles.len() as u64);
        run_pack(store, base, TileFormat::Jpeg, tiles, initial, cfg, cancel, &|_| {})
    }

    fn new_store() -> (tempfile::TempDir, HipsStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = HipsStore::new(dir.path(), &DSS_COLOR);
        (dir, store)
    }

    fn assert_accounted(p: &PackProgress) {
        assert_eq!(p.present + p.fetched + p.absent + p.failed, p.total, "{p:?}");
    }

    #[test]
    fn fetches_missing_and_skips_present() {
        let (_d, store) = new_store();
        let server = TestServer::all_ok();
        let tiles = tiles_of(10);
        for &(o, n) in &tiles[..3] {
            store.write(o, n, JPEG).unwrap();
        }
        let p = run(&store, &server.base, &tiles, &quick_cfg(), &AtomicBool::new(false));
        assert_eq!(p.state, PackState::Done, "{p:?}");
        assert_eq!((p.total, p.present, p.fetched, p.absent, p.failed), (10, 3, 7, 0, 0));
        assert_eq!(p.bytes_fetched, 7 * JPEG.len() as u64);
        assert_accounted(&p);
        for &(o, n) in &tiles {
            assert!(store.has(o, n), "tile {n}");
        }
        assert_eq!(server.total_hits(), 7, "stored tiles must not touch the network");
        assert_eq!(store.read(3, 8).unwrap(), crate::hips::CachedTile::Image(JPEG.to_vec()));
    }

    #[test]
    fn records_404_as_absent() {
        let (_d, store) = new_store();
        let server = TestServer::start(vec![(url_path(3, 1), vec![status(404, "")])], ok(), |_| {});
        let p = run(&store, &server.base, &tiles_of(3), &quick_cfg(), &AtomicBool::new(false));
        assert_eq!(p.state, PackState::Done);
        assert_eq!((p.fetched, p.absent, p.failed), (2, 1, 0));
        assert_eq!(store.read(3, 1).unwrap(), crate::hips::CachedTile::Absent);
        // A rerun does not ask again.
        let before = server.total_hits();
        let p2 = run(&store, &server.base, &tiles_of(3), &quick_cfg(), &AtomicBool::new(false));
        assert_eq!((p2.present, p2.fetched), (3, 0));
        assert_eq!(server.total_hits(), before);
    }

    #[test]
    fn cancel_keeps_finished_tiles() {
        let (_d, store) = new_store();
        let cancel = Arc::new(AtomicBool::new(false));
        let c2 = cancel.clone();
        let server = TestServer::start(Vec::new(), ok(), move |n| {
            if n == 3 {
                c2.store(true, Ordering::SeqCst);
            }
        });
        let tiles = tiles_of(40);
        let cfg = PackRunConfig { workers: 2, ..quick_cfg() };
        let p = run(&store, &server.base, &tiles, &cfg, &cancel);
        assert_eq!(p.state, PackState::Cancelled, "{p:?}");
        assert!(p.fetched >= 3 && p.fetched < 40, "{p:?}");
        assert_eq!(store.read_pack_state().unwrap().unwrap().state, PackState::Cancelled);

        // Resume: what was finished is present, the rest is fetched.
        let p2 = run(&store, &server.base, &tiles, &quick_cfg(), &AtomicBool::new(false));
        assert_eq!(p2.state, PackState::Done);
        assert!(p2.present >= 3, "{p2:?}");
        assert_eq!(p2.present, p.fetched + p.absent);
        assert_accounted(&p2);
    }

    #[test]
    fn busy_then_ok() {
        let (_d, store) = new_store();
        let server = TestServer::start(
            vec![(url_path(3, 0), vec![status(429, "Retry-After: 0\r\n"), ok()])],
            ok(),
            |_| {},
        );
        let p = run(&store, &server.base, &tiles_of(1), &quick_cfg(), &AtomicBool::new(false));
        assert_eq!(p.state, PackState::Done);
        assert_eq!((p.fetched, p.failed), (1, 0), "{p:?}");
        assert_eq!(server.hits_for(&url_path(3, 0)), 2);
        assert!(p.waiting_s.is_none());
    }

    #[test]
    fn busy_forever_fails_tile_after_max_waits() {
        let (_d, store) = new_store();
        let server = TestServer::start(Vec::new(), status(503, ""), |_| {});
        // One busy-failed tile must not count as a network error.
        let cfg = PackRunConfig { max_consecutive_errors: 1, ..quick_cfg() };
        let p = run(&store, &server.base, &tiles_of(2), &cfg, &AtomicBool::new(false));
        assert_eq!(p.state, PackState::Done, "{p:?}");
        assert_eq!((p.fetched, p.failed), (0, 2));
        assert!(p.error.is_none());
        // 1 first attempt + 6 retries per tile.
        assert_eq!(server.total_hits(), 2 * 7);
        assert!(!store.has(3, 0));
    }

    #[test]
    fn retry_after_is_capped_before_arithmetic() {
        let cfg = quick_cfg();
        assert_eq!(busy_wait(Some(Duration::from_secs(u64::MAX)), 0, &cfg), cfg.busy_cap);
        assert_eq!(busy_wait(Some(Duration::from_millis(7)), 3, &cfg), Duration::from_millis(7));
        // A zero or tiny Retry-After is floored at the backoff start.
        assert_eq!(busy_wait(Some(Duration::ZERO), 3, &cfg), cfg.backoff_start);
        assert_eq!(busy_wait(Some(Duration::ZERO), 0, &PackRunConfig::default()), Duration::from_secs(5));
        // No header: 1, 2, 4 ... ms, capped at 50 ms, never overflowing.
        assert_eq!(busy_wait(None, 0, &cfg), Duration::from_millis(1));
        assert_eq!(busy_wait(None, 2, &cfg), Duration::from_millis(4));
        assert_eq!(busy_wait(None, 30, &cfg), cfg.busy_cap);
        let real = PackRunConfig::default();
        assert_eq!(busy_wait(None, 0, &real), Duration::from_secs(5));
        assert_eq!(busy_wait(None, 5, &real), Duration::from_secs(160));
        assert_eq!(busy_wait(None, 6, &real), Duration::from_secs(300));
    }

    #[test]
    fn huge_retry_after_header_waits_only_the_cap() {
        let (_d, store) = new_store();
        let server = TestServer::start(
            vec![(url_path(3, 0), vec![status(429, "Retry-After: 18446744073709551615\r\n"), ok()])],
            ok(),
            |_| {},
        );
        let t = Instant::now();
        let p = run(&store, &server.base, &tiles_of(1), &quick_cfg(), &AtomicBool::new(false));
        assert_eq!((p.fetched, p.failed), (1, 0), "{p:?}");
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
    }

    #[test]
    fn cancel_during_a_busy_wait_returns_promptly() {
        let (_d, store) = new_store();
        let cancel = Arc::new(AtomicBool::new(false));
        let server = TestServer::start(Vec::new(), status(429, "Retry-After: 60\r\n"), |_| {});
        // The real cap: only the cancel can end the 60 s waits.
        let cfg = PackRunConfig { busy_cap: Duration::from_secs(300), ..quick_cfg() };
        let c2 = cancel.clone();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            c2.store(true, Ordering::SeqCst);
        });
        let t = Instant::now();
        let p = run(&store, &server.base, &tiles_of(4), &cfg, &cancel);
        let elapsed = t.elapsed();
        canceller.join().unwrap();
        assert_eq!(p.state, PackState::Cancelled, "{p:?}");
        assert_eq!(p.failed, 0);
        assert!(elapsed >= Duration::from_millis(250), "the cancel must arrive during the wait: {elapsed:?}");
        assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
        assert!(server.total_hits() >= 1, "the workers were waiting on a 429");
    }

    #[test]
    fn a_busy_wait_reaches_the_ui_while_it_lasts() {
        let (_d, store) = new_store();
        let server = TestServer::start(
            vec![(url_path(3, 0), vec![status(429, "Retry-After: 1\r\n"), ok()])],
            ok(),
            |_| {},
        );
        let cfg = PackRunConfig { busy_cap: Duration::from_secs(2), ..quick_cfg() };
        let seen = Mutex::new(Vec::new());
        let t = Instant::now();
        let initial = PackProgress::new(PackSelection::default(), 1);
        let p = run_pack(
            &store,
            &server.base,
            TileFormat::Jpeg,
            &tiles_of(1),
            initial,
            &cfg,
            &AtomicBool::new(false),
            &|q| seen.lock().unwrap().push((t.elapsed(), q.state, q.waiting_s)),
        );
        assert_eq!((p.fetched, p.failed), (1, 0), "{p:?}");
        let seen = seen.into_inner().unwrap();
        // The 429 comes ~ms after the start, inside the first throttle
        // interval; the held-back update must still be shown within about one
        // interval plus one sleep step.
        let shown = seen.iter().find(|(_, _, w)| *w == Some(1)).expect("waiting_s never shown");
        assert!(shown.0 < Duration::from_millis(800), "{:?}", seen);
        // And the end of the wait is shown while the run is still running.
        assert!(
            seen.iter().any(|(at, st, w)| *st == PackState::Running && w.is_none() && *at > shown.0),
            "{seen:?}"
        );
    }

    #[test]
    fn a_long_survey_outage_stops_the_run() {
        let (_d, store) = new_store();
        let server = TestServer::start(Vec::new(), status(503, ""), |_| {});
        let p = run(&store, &server.base, &tiles_of(40), &quick_cfg(), &AtomicBool::new(false));
        assert_eq!(p.state, PackState::Failed, "{p:?}");
        let err = p.error.as_deref().unwrap();
        assert!(err.starts_with("survey busy: "), "{err}");
        assert!(p.failed >= 4 && p.failed < 12, "the run must stop, not fail every tile: {p:?}");
        assert_eq!(store.read_pack_state().unwrap().unwrap().state, PackState::Failed);
    }

    #[test]
    fn a_success_resets_the_busy_streak() {
        let (_d, store) = new_store();
        // Odd tiles stay busy, even tiles are fine: never 2 busy tiles in a row.
        let routes = (0..20u64).filter(|n| n % 2 == 1).map(|n| (url_path(3, n), vec![status(503, "")])).collect();
        let server = TestServer::start(routes, ok(), |_| {});
        let cfg = PackRunConfig { workers: 1, max_consecutive_busy_failures: 2, ..quick_cfg() };
        let p = run(&store, &server.base, &tiles_of(20), &cfg, &AtomicBool::new(false));
        assert_eq!(p.state, PackState::Done, "{p:?}");
        assert_eq!((p.fetched, p.failed), (10, 10));
    }

    #[test]
    fn offline_stops_after_consecutive_errors() {
        let (_d, store) = new_store();
        // The server accepts and hangs up without answering: a plain request
        // error on every tile, with no freed-port race.
        let server = TestServer::start(Vec::new(), hang_up(), |_| {});
        let tiles = tiles_of(200);
        store.write(3, 199, JPEG).unwrap();
        let p = run(&store, &server.base, &tiles, &quick_cfg(), &AtomicBool::new(false));
        assert_eq!(p.state, PackState::Failed, "{p:?}");
        assert!(p.failed >= 20 && p.failed < 20 + 4 + 1, "{p:?}");
        assert!(p.failed < 100, "the run must stop, not fail every tile: {p:?}");
        let err = p.error.as_deref().unwrap();
        assert!(err.contains("in a row") && err.contains(&server.addr.to_string()), "{err}");
        assert_eq!(store.read_pack_state().unwrap().unwrap().state, PackState::Failed);
        assert!(store.has(3, 199), "finished tiles are kept");
    }

    #[test]
    fn a_success_resets_the_error_streak() {
        let (_d, store) = new_store();
        // Odd tiles answer 500, even tiles are fine: never 2 errors in a row.
        let routes = (0..20u64).filter(|n| n % 2 == 1).map(|n| (url_path(3, n), vec![status(500, "")])).collect();
        let server = TestServer::start(routes, ok(), |_| {});
        let cfg = PackRunConfig { workers: 1, max_consecutive_errors: 2, ..quick_cfg() };
        let p = run(&store, &server.base, &tiles_of(20), &cfg, &AtomicBool::new(false));
        assert_eq!(p.state, PackState::Done, "{p:?}");
        assert_eq!((p.fetched, p.failed), (10, 10));
    }

    #[test]
    fn interrupted_after_restart() {
        let (_d, store) = new_store();
        assert_eq!(stored_status(&store).state, PackState::Idle);
        let selection = PackSelection { sky_order: 3, around_ngc_ic: true, ..PackSelection::default() };
        let running = PackProgress { fetched: 7, total: 20, ..PackProgress::new(selection.clone(), 20) };
        store.write_pack_state(&running).unwrap();
        let p = stored_status(&store);
        assert_eq!(p.state, PackState::Interrupted);
        assert_eq!((p.fetched, p.total), (7, 20), "counts survive the restart");
        let got = p.selection.unwrap();
        assert_eq!((got.sky_order, got.around_ngc_ic, got.survey_id), (3, true, selection.survey_id));
        // A finished pack is not interrupted and keeps its counts.
        store
            .write_pack_state(&PackProgress { state: PackState::Done, fetched: 20, total: 20, ..Default::default() })
            .unwrap();
        let p = stored_status(&store);
        assert_eq!((p.state, p.fetched), (PackState::Done, 20));
    }

    #[test]
    fn pack_json_of_an_older_build_and_a_corrupt_one() {
        let (_d, store) = new_store();
        std::fs::create_dir_all(store.root()).unwrap();
        // The first build wrote selection and state only.
        std::fs::write(store.root().join("pack.json"), br#"{"selection":null,"state":"failed"}"#).unwrap();
        let p = stored_status(&store);
        assert_eq!((p.state, p.fetched, p.total), (PackState::Failed, 0, 0));
        // A corrupt file reads as idle, not as an error.
        std::fs::write(store.root().join("pack.json"), b"{not json").unwrap();
        assert_eq!(stored_status(&store).state, PackState::Idle);
    }

    #[test]
    fn pack_json_says_running_during_the_run_and_the_end_state_after() {
        let (_d, store) = new_store();
        let server = TestServer::all_ok();
        let seen = Mutex::new(Vec::new());
        let initial = PackProgress::new(PackSelection::default(), 5);
        let p = run_pack(
            &store,
            &server.base,
            TileFormat::Jpeg,
            &tiles_of(5),
            initial,
            &quick_cfg(),
            &AtomicBool::new(false),
            &|q| seen.lock().unwrap().push((q.state, store.read_pack_state().unwrap().unwrap().state)),
        );
        let seen = seen.into_inner().unwrap();
        assert_eq!(seen.first().unwrap(), &(PackState::Running, PackState::Running));
        assert_eq!(seen.last().unwrap(), &(PackState::Done, PackState::Done));
        assert_eq!(p.state, PackState::Done);
        let stored = stored_status(&store);
        assert_eq!((stored.state, stored.fetched), (PackState::Done, 5), "pack.json keeps the counts");
    }

    #[test]
    fn progress_callbacks_are_throttled_with_a_final_one() {
        let (_d, store) = new_store();
        let server = TestServer::all_ok();
        let calls = AtomicUsize::new(0);
        let finals = AtomicUsize::new(0);
        let initial = PackProgress::new(PackSelection::default(), 60);
        let t = Instant::now();
        run_pack(
            &store,
            &server.base,
            TileFormat::Jpeg,
            &tiles_of(60),
            initial,
            &quick_cfg(),
            &AtomicBool::new(false),
            &|q| {
                calls.fetch_add(1, Ordering::SeqCst);
                if q.state == PackState::Done {
                    finals.fetch_add(1, Ordering::SeqCst);
                }
            },
        );
        assert_eq!(finals.load(Ordering::SeqCst), 1);
        // The start and the final, plus at most one per throttle interval
        // that passed (a loaded machine may take longer than a second).
        let elapsed = t.elapsed();
        let n = calls.load(Ordering::SeqCst);
        assert!(n >= 2 && n as u128 <= 3 + elapsed.as_millis() / 250, "{n} callbacks in {elapsed:?}");
    }
}

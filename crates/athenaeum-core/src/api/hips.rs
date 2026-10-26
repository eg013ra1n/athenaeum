//! Sky Chart background commands (issue #18, phase 1): the survey list,
//! one tile by address, and clearing a survey's private tile store.
//!
//! A tile is served from the store; a missing one is fetched from the
//! survey's master only when `skychart.hips_online` is on. Nothing here
//! downloads anything a user did not scroll to.
//!
//! Phase 2 adds the offline pack: estimate, start, cancel and status of one
//! background download per process. An explicit pack works with
//! `skychart.hips_online` off; the toggle stays the on-demand switch only.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use serde::Serialize;

use super::ApiError;
use crate::hips::{self, CachedTile, FetchedTile, HipsStore, HipsSurvey, Properties};
use crate::services::ServiceContext;
use crate::settings::{defaults, keys};

/// One survey as the Sky Chart and the Settings page show it.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct HipsSurveyInfo {
    pub id: String,
    pub label: String,
    /// `image/jpeg` or `image/png`.
    pub mime: String,
    /// Deepest order the chart may request (the survey's, capped at 6).
    pub max_order: u8,
    pub tile_width: u32,
    /// The survey's `obs_copyright`, or the preset's until `properties` were read.
    pub attribution: String,
    pub attribution_url: Option<String>,
    /// `hips_license`, or the preset's.
    pub license: String,
    /// Online fetching is on (`skychart.hips_online`).
    pub online: bool,
    pub cached_tiles: u64,
    pub cached_bytes: u64,
}

/// A tile as served to the chart.
pub enum HipsTile {
    Image { bytes: Vec<u8>, mime: &'static str },
    /// No coverage at this address (the master answered 404).
    Absent,
}

fn app_data_dir(ctx: &ServiceContext) -> Result<PathBuf, ApiError> {
    let db = ctx.db.get().ok_or_else(|| ApiError::Internal("Database not initialized".into()))?;
    db.path()
        .parent()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| ApiError::Internal("Cannot determine the app data directory".into()))
}

fn online(ctx: &ServiceContext) -> Result<bool, ApiError> {
    let db = ctx.db.get().ok_or_else(|| ApiError::Internal("Database not initialized".into()))?;
    let conn = db.conn();
    let v = ctx.settings.get_with_precedence(&conn, keys::SKYCHART_HIPS_ONLINE, defaults::SKYCHART_HIPS_ONLINE)?;
    Ok(v != "false")
}

fn find_survey(id: &str) -> Result<&'static HipsSurvey, ApiError> {
    hips::survey(id).ok_or_else(|| ApiError::NotFound(format!("Unknown sky survey: {id}")))
}

/// Surveys whose stored `properties` passed validation in this process.
fn validated() -> &'static Mutex<HashSet<&'static str>> {
    static V: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    V.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Makes sure the survey's `properties` are on disk and drawable; fetches
/// them from the master on first use when online.
fn ensure_properties(store: &HipsStore, survey: &'static HipsSurvey, online: bool) -> Result<(), ApiError> {
    if validated().lock().unwrap().contains(survey.id) {
        return Ok(());
    }
    let props = match store.read_properties()? {
        Some(p) => p,
        None if online => {
            let text = hips::fetch_properties(survey).map_err(|e| {
                tracing::warn!(survey_id = survey.id, error = %format!("{e:#}"), "hips properties fetch failed");
                ApiError::Internal(format!("Could not reach {} to read its survey description: {e:#}", survey.label))
            })?;
            let p = Properties::parse(&text);
            p.validate(survey).map_err(|e| {
                tracing::error!(survey_id = survey.id, error = %format!("{e:#}"), "hips survey properties refused");
                ApiError::Conflict(format!("{e:#}"))
            })?;
            store.write_properties(&text)?;
            p
        }
        None => {
            return Err(ApiError::NotFound(format!(
                "{} has no local tiles yet and online fetching is off",
                survey.label
            )))
        }
    };
    props.validate(survey).map_err(|e| {
        tracing::error!(survey_id = survey.id, error = %format!("{e:#}"), "hips survey properties refused");
        ApiError::Conflict(format!("{e:#}"))
    })?;
    validated().lock().unwrap().insert(survey.id);
    Ok(())
}

pub fn get_hips_surveys(ctx: &ServiceContext) -> Result<Vec<HipsSurveyInfo>, ApiError> {
    let app_data = app_data_dir(ctx)?;
    let online = online(ctx)?;
    hips::SURVEYS
        .iter()
        .map(|s| {
            let store = HipsStore::new(&app_data, s);
            let props = store.read_properties()?.unwrap_or_default();
            let (cached_tiles, cached_bytes) = store.usage()?;
            Ok(HipsSurveyInfo {
                id: s.id.to_string(),
                label: s.label.to_string(),
                mime: s.format.mime().to_string(),
                max_order: hips::request_max_order(s),
                tile_width: hips::TILE_WIDTH,
                attribution: props.get("obs_copyright").unwrap_or(s.fallback_attribution).to_string(),
                attribution_url: props.get("obs_copyright_url").map(str::to_string),
                license: props.get("hips_license").unwrap_or(s.fallback_license).to_string(),
                online,
                cached_tiles,
                cached_bytes,
            })
        })
        .collect()
}

/// One tile: from the store, else (online) from the survey's master, which
/// is then stored. Offline and not stored is `NotFound`; a fetch failure is
/// `Internal` and is not remembered, so a later request tries again.
pub fn get_hips_tile(ctx: &ServiceContext, survey_id: &str, order: u8, npix: u64) -> Result<HipsTile, ApiError> {
    let survey = find_survey(survey_id)?;
    hips::validate_address(survey, order, npix).map_err(|e| ApiError::Invalid(format!("{e:#}")))?;
    let store = HipsStore::new(&app_data_dir(ctx)?, survey);
    let mime = survey.format.mime();

    match store.read(order, npix)? {
        CachedTile::Image(bytes) => return Ok(HipsTile::Image { bytes, mime }),
        CachedTile::Absent => return Ok(HipsTile::Absent),
        CachedTile::Missing => {}
    }
    let online = online(ctx)?;
    if !online {
        return Err(ApiError::NotFound(format!("Tile {order}/{npix} is not cached and online fetching is off")));
    }
    ensure_properties(&store, survey, online)?;
    match hips::fetch_tile(survey, order, npix) {
        Ok(FetchedTile::Image(bytes)) => {
            tracing::debug!(survey_id = survey.id, hips_order = order, npix, bytes = bytes.len(), "hips tile fetched");
            store.write(order, npix, &bytes)?;
            Ok(HipsTile::Image { bytes, mime })
        }
        Ok(FetchedTile::Absent) => {
            tracing::debug!(survey_id = survey.id, hips_order = order, npix, "hips tile absent");
            store.mark_absent(order, npix)?;
            Ok(HipsTile::Absent)
        }
        Err(e) => {
            tracing::warn!(survey_id = survey.id, hips_order = order, npix, error = %format!("{e:#}"), "hips tile fetch failed");
            Err(ApiError::Internal(format!("{e:#}")))
        }
    }
}

/// Runs `f` while no pack of `survey_id` is live, with the pack slot locked so
/// none can start meanwhile. `None` when a pack of that survey is live.
#[cfg(all(feature = "render", feature = "solver"))]
fn while_pack_idle<T>(survey_id: &str, f: impl FnOnce() -> T) -> Option<T> {
    offline_pack::while_idle(survey_id, f)
}

/// This build has no pack: nothing can be live.
#[cfg(not(all(feature = "render", feature = "solver")))]
fn while_pack_idle<T>(_survey_id: &str, f: impl FnOnce() -> T) -> Option<T> {
    Some(f())
}

/// A rename refused because something holds a handle below the folder. Only
/// Windows refuses a rename for an open file (`PermissionDenied`, or
/// ERROR_SHARING_VIOLATION, os error 32); elsewhere a refusal is a real
/// permission problem that waiting will not fix.
fn is_busy_folder(e: &anyhow::Error) -> bool {
    cfg!(windows)
        && e.chain().filter_map(|c| c.downcast_ref::<std::io::Error>()).any(|io| {
            io.kind() == std::io::ErrorKind::PermissionDenied || io.raw_os_error() == Some(32)
        })
}

/// Removes tombstones (`<root>.clearing-*`) a crashed Clear left behind.
/// This process's own tombstones are skipped: a concurrent Clear may still be
/// sizing one, and a later launch sweeps them.
/// Best effort: a failure is logged and never fails the Clear.
fn sweep_tombstones(store: &HipsStore) {
    let (Some(dir), Some(name)) = (store.root().parent(), store.root().file_name()) else { return };
    let prefix = format!("{}.clearing-", name.to_string_lossy());
    let own = format!("{prefix}{}-", std::process::id());
    let entries = match std::fs::read_dir(dir) {
        Ok(it) => it,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            tracing::warn!(error = %e, "hips tombstone sweep: cannot list the folder");
            return;
        }
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if !file_name.starts_with(&prefix) || file_name.starts_with(&own) {
            continue;
        }
        let path = entry.path();
        if let Err(e) = std::fs::remove_dir_all(&path) {
            tracing::warn!(path = %path.display(), error = %e, "hips stale tombstone not removed");
        }
    }
}

/// Deletes a survey's whole tile store; returns the tile bytes it held.
/// Refused while a pack of that survey runs: the store would be half-written.
///
/// The store is renamed to a tombstone under the pack lock (O(1)) and sized
/// and deleted after the lock is released, so a large store never stalls the
/// pack commands.
pub fn clear_hips_cache(ctx: &ServiceContext, survey_id: &str) -> Result<u64, ApiError> {
    let survey = find_survey(survey_id)?;
    let store = HipsStore::new(&app_data_dir(ctx)?, survey);
    sweep_tombstones(&store);
    let moved = while_pack_idle(survey.id, || {
        let moved = store.move_aside()?;
        // Under the pack lock: a start in between cannot skip the properties fetch.
        validated().lock().unwrap().remove(survey.id);
        Ok::<_, anyhow::Error>(moved)
    })
    .ok_or_else(|| {
        tracing::warn!(survey_id = survey.id, "hips cache clear refused while a pack runs");
        ApiError::Conflict("a sky pack download is running".into())
    })?
    .map_err(|e| {
        tracing::error!(survey_id = survey.id, error = %format!("{e:#}"), "hips cache move aside failed");
        if is_busy_folder(&e) {
            ApiError::Conflict("Some sky tiles are in use; try again in a moment".into())
        } else {
            ApiError::from(e)
        }
    })?;
    let bytes = match moved {
        Some(tomb) => tomb.clear().map_err(|e| {
            tracing::error!(survey_id = survey.id, error = %format!("{e:#}"), "hips cache removal failed");
            e
        })?,
        None => 0,
    };
    tracing::info!(survey_id = survey.id, bytes, "hips cache cleared");
    Ok(bytes)
}

#[cfg(all(feature = "render", feature = "solver"))]
pub use offline_pack::{
    cancel_hips_pack, estimate_hips_pack, get_hips_pack_status, start_hips_pack, HipsPackEstimate,
};

#[cfg(all(feature = "render", feature = "solver"))]
mod offline_pack {
    use std::panic::AssertUnwindSafe;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use super::*;
    use crate::events::{emit_event, ProgressEmitter};
    use crate::hips::pack::{self, PackProgress, PackRunConfig, PackSelection, PackState};

    /// A download keeps this much free on the disk.
    const DISK_RESERVE_BYTES: u64 = 512 * 1024 * 1024;

    /// What a pack would download, before it starts. Counts only; no network.
    #[derive(Debug, Clone, Serialize, ts_rs::TS)]
    #[serde(rename_all = "camelCase")]
    pub struct HipsPackEstimate {
        pub total: u64,
        /// Tiles already stored (images or 404 markers).
        pub present: u64,
        pub missing: u64,
        pub bytes_per_tile: u64,
        /// `missing * bytes_per_tile`.
        pub estimated_bytes: u64,
        pub free_bytes: Option<u64>,
        /// The download plus a 512 MiB reserve fits the free space (true when the
        /// free space is unknown).
        pub fits: bool,
    }

    /// The one pack of this process. Held in a static rather than in
    /// `ServiceContext`: one pack per process is the rule, and the context has
    /// many construction sites.
    struct LivePack {
        survey_id: String,
        cancel: Arc<AtomicBool>,
        /// Latest snapshot. Written by the run's progress callback, which runs
        /// under the run's own progress mutex: it must only store and emit, never
        /// lock the slot.
        latest: Arc<Mutex<PackProgress>>,
    }

    impl LivePack {
        /// A run that has emitted its end state is over; only its thread's
        /// guard has yet to free the slot. Lock order is slot, then `latest`;
        /// the progress callback never takes the slot.
        fn is_live(&self) -> bool {
            let state = self.latest.lock().unwrap_or_else(|e| e.into_inner()).state;
            !matches!(state, PackState::Done | PackState::Cancelled | PackState::Failed)
        }
    }

    fn pack_slot() -> &'static Mutex<Option<LivePack>> {
        static P: OnceLock<Mutex<Option<LivePack>>> = OnceLock::new();
        P.get_or_init(|| Mutex::new(None))
    }

    /// Empties the slot on every exit of the pack thread, a panic included, but
    /// only if the slot still holds this run.
    struct SlotGuard(Arc<AtomicBool>);

    impl Drop for SlotGuard {
        fn drop(&mut self) {
            let mut slot = pack_slot().lock().unwrap_or_else(|e| e.into_inner());
            if slot.as_ref().is_some_and(|l| Arc::ptr_eq(&l.cancel, &self.0)) {
                *slot = None;
            }
        }
    }

    fn pack_is_live() -> bool {
        pack_slot().lock().unwrap().as_ref().is_some_and(LivePack::is_live)
    }

    /// See [`super::while_pack_idle`]. A slot holding a finished run counts as
    /// free.
    pub(super) fn while_idle<T>(survey_id: &str, f: impl FnOnce() -> T) -> Option<T> {
        let slot = pack_slot().lock().unwrap();
        if slot.as_ref().is_some_and(|l| l.survey_id == survey_id && l.is_live()) {
            return None;
        }
        Some(f())
    }

    fn live_conflict() -> ApiError {
        ApiError::Conflict("a sky pack download is already running".into())
    }

    /// Validates the selection and lists its tiles.
    fn selected_tiles(
        ctx: &ServiceContext,
        selection: &PackSelection,
    ) -> Result<(&'static HipsSurvey, Vec<(u8, u64)>), ApiError> {
        let survey = find_survey(&selection.survey_id)?;
        pack::checked_sky_order(selection.sky_order).map_err(|e| ApiError::Invalid(format!("{e:#}")))?;
        let mut regions = Vec::new();
        if selection.around_locations {
            let db = ctx.db.get().ok_or_else(|| ApiError::Internal("Database not initialized".into()))?;
            let conn = db.conn();
            let locations = super::super::spatial::get_imaging_locations(&conn)?;
            regions.extend(pack::location_regions(&locations));
        }
        if selection.around_messier || selection.around_ngc_ic {
            let catalog = super::super::objects::load_catalog(ctx)
                .ok_or_else(|| ApiError::Internal("The deep-sky catalog could not be loaded".into()))?;
            regions.extend(pack::catalog_regions(&catalog, selection.around_messier, selection.around_ngc_ic));
        }
        let tiles = pack::pack_tiles(selection.sky_order, &regions).map_err(|e| ApiError::Invalid(format!("{e:#}")))?;
        Ok((survey, tiles))
    }

    /// Size of one tile: the survey's own `hips_estsize` (KB) over
    /// `hips_nb_tiles`, else the preset's typical size.
    fn bytes_per_tile(store: &HipsStore, survey: &HipsSurvey) -> Result<u64, ApiError> {
        let props = store.read_properties()?;
        let number = |key: &str| props.as_ref().and_then(|p| p.get(key)).and_then(|v| v.parse::<u64>().ok());
        Ok(match (number("hips_estsize"), number("hips_nb_tiles")) {
            (Some(kb), Some(n)) if kb > 0 && n > 0 => (kb.saturating_mul(1024) / n).max(1),
            _ => survey.approx_tile_bytes,
        })
    }

    /// The download plus the reserve fits the free space (true when unknown).
    fn fits_disk(estimated_bytes: u64, free_bytes: Option<u64>) -> bool {
        free_bytes.map_or(true, |f| estimated_bytes.saturating_add(DISK_RESERVE_BYTES) <= f)
    }

    /// Megabytes to 0.1, so a small pack never reads "0 MB".
    fn format_mb(bytes: u64) -> String {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }

    /// Why the disk refuses a pack, or `None`. A pack with nothing left to
    /// download needs no space.
    fn disk_refusal(estimate: &HipsPackEstimate) -> Option<String> {
        if estimate.missing == 0 || estimate.fits {
            return None;
        }
        Some(format!(
            "Needs about {} plus a 512 MB reserve; {} free",
            format_mb(estimate.estimated_bytes),
            format_mb(estimate.free_bytes.unwrap_or(0))
        ))
    }

    fn estimate_for(
        app_data: &std::path::Path,
        store: &HipsStore,
        survey: &HipsSurvey,
        tiles: &[(u8, u64)],
    ) -> Result<HipsPackEstimate, ApiError> {
        let total = tiles.len() as u64;
        let present = tiles.iter().filter(|&&(o, n)| store.has(o, n)).count() as u64;
        let missing = total - present;
        let bytes_per_tile = bytes_per_tile(store, survey)?;
        let estimated_bytes = missing.saturating_mul(bytes_per_tile);
        let free_bytes = crate::disk::free_bytes(app_data);
        let fits = fits_disk(estimated_bytes, free_bytes);
        Ok(HipsPackEstimate { total, present, missing, bytes_per_tile, estimated_bytes, free_bytes, fits })
    }

    /// What the selection would download. Reads the catalog and the disk only;
    /// it never touches the network.
    pub fn estimate_hips_pack(ctx: &ServiceContext, selection: &PackSelection) -> Result<HipsPackEstimate, ApiError> {
        let (survey, tiles) = selected_tiles(ctx, selection)?;
        let app_data = app_data_dir(ctx)?;
        let store = HipsStore::new(&app_data, survey);
        estimate_for(&app_data, &store, survey, &tiles)
    }

    /// Runs `run`; if it panics, logs it and ends the pack as `Failed`: the
    /// snapshot is stored, written to `pack.json` and emitted, so the UI does
    /// not wait on a run that is gone.
    fn run_catching_panic(
        survey_id: &str,
        store: &HipsStore,
        latest: &Mutex<PackProgress>,
        on_progress: &dyn Fn(&PackProgress),
        run: impl FnOnce(),
    ) {
        if std::panic::catch_unwind(AssertUnwindSafe(run)).is_ok() {
            return;
        }
        tracing::error!(survey_id, "hips pack panicked");
        let mut failed = latest.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if matches!(failed.state, PackState::Done | PackState::Cancelled | PackState::Failed) {
            // The run had already finished (pack.json written, slot free): a new
            // run may own the store now, so this late panic rewrites nothing.
            return;
        }
        failed.state = PackState::Failed;
        failed.waiting_s = None;
        failed.error = Some("internal error".into());
        if let Err(e) = store.write_pack_state(&failed) {
            tracing::warn!(survey_id, error = %format!("{e:#}"), "hips pack state not saved");
        }
        on_progress(&failed);
    }

    /// Starts the pack on a background thread and returns its first snapshot.
    /// Progress goes out as `hips-download-progress`.
    pub fn start_hips_pack(
        ctx: Arc<ServiceContext>,
        emitter: Arc<dyn ProgressEmitter>,
        selection: PackSelection,
    ) -> Result<PackProgress, ApiError> {
        let (survey, tiles) = selected_tiles(&ctx, &selection)?;
        if pack_is_live() {
            return Err(live_conflict());
        }
        let app_data = app_data_dir(&ctx)?;
        let store = HipsStore::new(&app_data, survey);
        // An explicit pack fetches the survey's `properties` even with online
        // fetching off: the toggle is the on-demand switch only.
        ensure_properties(&store, survey, true)?;
        let estimate = estimate_for(&app_data, &store, survey, &tiles)?;
        if let Some(msg) = disk_refusal(&estimate) {
            tracing::warn!(survey_id = survey.id, estimated_bytes = estimate.estimated_bytes, "hips pack refused: disk");
            return Err(ApiError::Conflict(msg));
        }

        let mut initial = PackProgress::new(selection, estimate.total);
        initial.present = estimate.present;
        let cancel = Arc::new(AtomicBool::new(false));
        let latest = Arc::new(Mutex::new(initial.clone()));
        {
            // Claimed before the thread exists: two racing starts cannot both
            // run. A slot holding a finished run is replaced; that run's guard
            // only clears a slot that still holds it.
            let mut slot = pack_slot().lock().unwrap();
            if slot.as_ref().is_some_and(LivePack::is_live) {
                return Err(live_conflict());
            }
            *slot = Some(LivePack { survey_id: survey.id.to_string(), cancel: cancel.clone(), latest: latest.clone() });
        }

        let thread_initial = initial.clone();
        let thread_cancel = cancel.clone();
        let spawned = std::thread::Builder::new().name("hips-pack".into()).spawn(move || {
            let _guard = SlotGuard(thread_cancel.clone());
            let on_progress = |p: &PackProgress| {
                *latest.lock().unwrap_or_else(|e| e.into_inner()) = p.clone();
                emit_event(&*emitter, "hips-download-progress", p);
            };
            let store = HipsStore::new(&app_data, survey);
            run_catching_panic(survey.id, &store, &latest, &on_progress, || {
                pack::run_pack(
                    &store,
                    survey.base_url,
                    survey.format,
                    &tiles,
                    thread_initial,
                    &PackRunConfig::default(),
                    &thread_cancel,
                    &on_progress,
                );
            });
        });
        if let Err(e) = spawned {
            let mut slot = pack_slot().lock().unwrap();
            if slot.as_ref().is_some_and(|l| Arc::ptr_eq(&l.cancel, &cancel)) {
                *slot = None;
            }
            tracing::error!(survey_id = survey.id, error = %e, "hips pack thread failed to start");
            return Err(ApiError::Internal(format!("could not start the pack thread: {e}")));
        }
        Ok(initial)
    }

    /// Asks the running pack to stop. In-flight requests finish; the finished
    /// tiles stay. `NotFound` when nothing runs.
    pub fn cancel_hips_pack() -> Result<(), ApiError> {
        let slot = pack_slot().lock().unwrap();
        match slot.as_ref() {
            Some(live) => {
                live.cancel.store(true, Ordering::Relaxed);
                tracing::info!(survey_id = %live.survey_id, "hips pack cancel requested");
                Ok(())
            }
            None => Err(ApiError::NotFound("no sky pack download is running".into())),
        }
    }

    /// The live run's latest snapshot, else what `pack.json` last stored
    /// (counts included; `interrupted` if the app died mid-run), else `idle`.
    pub fn get_hips_pack_status(ctx: &ServiceContext, survey_id: &str) -> Result<PackProgress, ApiError> {
        let survey = find_survey(survey_id)?;
        let live = {
            let slot = pack_slot().lock().unwrap();
            slot.as_ref().filter(|l| l.survey_id == survey.id).map(|l| (l.latest.clone(), l.cancel.clone()))
        };
        if let Some((latest, cancel)) = live {
            let mut p = latest.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if p.state == PackState::Running && cancel.load(Ordering::Relaxed) {
                p.state = PackState::Cancelling;
            }
            return Ok(p);
        }
        let store = HipsStore::new(&app_data_dir(ctx)?, survey);
        Ok(pack::stored_status(&store))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::events::NullEmitter;
        use crate::hips::{TileFormat, DSS_COLOR};

        fn test_ctx() -> (tempfile::TempDir, ServiceContext) {
            let dir = tempfile::tempdir().unwrap();
            let ctx = ServiceContext::new_for_tests(dir.path().join("catalog.db"));
            (dir, ctx)
        }

        fn sky_only(sky_order: u8) -> PackSelection {
            PackSelection {
                sky_order,
                around_locations: false,
                around_messier: false,
                around_ngc_ic: false,
                ..PackSelection::default()
            }
        }

        /// A JPEG-looking tile body.
        fn jpeg() -> Vec<u8> {
            assert!(TileFormat::Jpeg.matches(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0]));
            vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 0, 0, 0]
        }

        #[test]
        fn estimate_is_offline_and_counts_present() {
            let (dir, ctx) = test_ctx();
            let store = HipsStore::new(dir.path(), &DSS_COLOR);
            for npix in 0..5 {
                store.write(0, npix, &jpeg()).unwrap();
            }
            let e = estimate_hips_pack(&ctx, &sky_only(0)).unwrap();
            assert_eq!((e.total, e.present, e.missing, e.bytes_per_tile), (12, 5, 7, 67_000));
            assert_eq!(e.estimated_bytes, 7 * 67_000);
            // The rule, not the test machine's free space.
            assert_eq!(e.fits, e.free_bytes.map_or(true, |f| e.estimated_bytes + DISK_RESERVE_BYTES <= f));
        }

        #[test]
        fn disk_rule_and_message() {
            const MIB: u64 = 1024 * 1024;
            assert!(fits_disk(100, None), "unknown free space fits");
            assert!(fits_disk(100 * MIB, Some(612 * MIB)));
            assert!(!fits_disk(100 * MIB, Some(611 * MIB)));
            assert!(!fits_disk(u64::MAX, Some(u64::MAX - 1)), "the sum saturates, never wraps");

            let est = |missing, estimated_bytes, free_bytes: Option<u64>| HipsPackEstimate {
                total: 10,
                present: 10 - missing,
                missing,
                bytes_per_tile: 1,
                estimated_bytes,
                free_bytes,
                fits: fits_disk(estimated_bytes, free_bytes),
            };
            // Nothing to download: no space needed, however little is free.
            assert_eq!(disk_refusal(&est(0, 0, Some(0))), None);
            // A small pack reads in tenths of a MB, never "0 MB".
            let msg = disk_refusal(&est(2, 150_000, Some(100 * MIB))).unwrap();
            assert_eq!(msg, "Needs about 0.1 MB plus a 512 MB reserve; 100.0 MB free");
            assert_eq!(disk_refusal(&est(2, 150_000, Some(2048 * MIB))), None);
        }

        #[test]
        fn estimate_uses_properties_size() {
            let (dir, ctx) = test_ctx();
            let store = HipsStore::new(dir.path(), &DSS_COLOR);
            store.write_properties("hips_estsize = 100\nhips_nb_tiles = 10\n").unwrap();
            let e = estimate_hips_pack(&ctx, &sky_only(0)).unwrap();
            assert_eq!(e.bytes_per_tile, 10_240);
        }

        #[test]
        fn no_locations_and_no_catalog_is_whole_sky_only() {
            // Review Focus 5: an empty catalog and both boxes off is not an error.
            let (_dir, ctx) = test_ctx();
            let sel = PackSelection { around_locations: true, ..sky_only(1) };
            let e = estimate_hips_pack(&ctx, &sel).unwrap();
            assert_eq!(e.total, 12 + 48);
        }

        #[test]
        fn start_refuses_order_6() {
            let (_dir, ctx) = test_ctx();
            let err = start_hips_pack(Arc::new(ctx), Arc::new(NullEmitter), sky_only(6)).unwrap_err();
            assert!(matches!(err, ApiError::Invalid(_)), "{err:?}");
            let (_dir, ctx) = test_ctx();
            let err = estimate_hips_pack(&ctx, &sky_only(6)).unwrap_err();
            assert!(matches!(err, ApiError::Invalid(_)), "{err:?}");
        }

        #[test]
        fn unknown_survey_is_not_found() {
            let (_dir, ctx) = test_ctx();
            let sel = PackSelection { survey_id: "nope".into(), ..sky_only(0) };
            assert!(matches!(estimate_hips_pack(&ctx, &sel), Err(ApiError::NotFound(_))));
            assert!(matches!(get_hips_pack_status(&ctx, "nope"), Err(ApiError::NotFound(_))));
        }

        #[test]
        fn a_panicking_run_ends_failed_and_is_emitted() {
            let dir = tempfile::tempdir().unwrap();
            let store = HipsStore::new(dir.path(), &DSS_COLOR);
            let mut running = PackProgress::new(sky_only(0), 12);
            running.fetched = 4;
            let latest = Mutex::new(running);
            let seen = Mutex::new(Vec::new());
            run_catching_panic(
                "dss-color",
                &store,
                &latest,
                &|p| seen.lock().unwrap().push(p.clone()),
                || panic!("worker blew up"),
            );
            let seen = seen.into_inner().unwrap();
            assert_eq!(seen.len(), 1);
            assert_eq!((seen[0].state, seen[0].fetched), (PackState::Failed, 4));
            assert_eq!(seen[0].error.as_deref(), Some("internal error"));
            let stored = pack::stored_status(&store);
            assert_eq!((stored.state, stored.fetched), (PackState::Failed, 4));
            // A run that does not panic emits nothing here.
            run_catching_panic("dss-color", &store, &latest, &|_| panic!("unexpected"), || {});
        }

        #[test]
        fn a_late_panic_after_a_terminal_state_rewrites_nothing() {
            let dir = tempfile::tempdir().unwrap();
            let store = HipsStore::new(dir.path(), &DSS_COLOR);
            let mut done = PackProgress::new(sky_only(0), 12);
            done.state = PackState::Done;
            let latest = Mutex::new(done);
            run_catching_panic("dss-color", &store, &latest, &|_| panic!("must not emit"), || panic!("late"));
            assert_eq!(pack::stored_status(&store).state, PackState::Idle, "pack.json is not written");
        }

        #[test]
        fn clear_sweeps_stale_tombstones_and_keeps_other_surveys() {
            let (_dir, ctx) = test_ctx();
            let root = app_data_dir(&ctx).unwrap().join("hips");
            let stale = root.join("dss-color.clearing-1-0");
            let other = root.join("other.clearing-1-0");
            std::fs::create_dir_all(&stale).unwrap();
            std::fs::create_dir_all(&other).unwrap();
            clear_hips_cache(&ctx, "dss-color").unwrap();
            assert!(!stale.exists());
            assert!(other.exists());
        }

        /// Run first inside `pack_slot_rules`: it reads the process-wide slot.
        fn status_idle_without_pack_json() {
            let (_dir, ctx) = test_ctx();
            let p = get_hips_pack_status(&ctx, "dss-color").unwrap();
            assert_eq!(p.state, PackState::Idle);
            assert!(p.selection.is_none());
        }

        /// Run inside `pack_slot_rules`, with the slot empty.
        fn status_reports_stored_state_with_counts() {
            let (dir, ctx) = test_ctx();
            let store = HipsStore::new(dir.path(), &DSS_COLOR);
            let sel = PackSelection::default();
            let mut running = PackProgress::new(sel.clone(), 20);
            running.fetched = 6;
            store.write_pack_state(&running).unwrap();
            let p = get_hips_pack_status(&ctx, "dss-color").unwrap();
            assert_eq!((p.state, p.fetched), (PackState::Interrupted, 6));
            assert_eq!(p.selection.unwrap().sky_order, sel.sky_order);

            let done = PackProgress { state: PackState::Done, fetched: 18, absent: 2, ..PackProgress::new(sel, 20) };
            store.write_pack_state(&done).unwrap();
            let p = get_hips_pack_status(&ctx, "dss-color").unwrap();
            assert_eq!((p.state, p.total, p.fetched, p.absent), (PackState::Done, 20, 18, 2));
            assert!(p.selection.is_some());

            // A corrupt pack.json is idle, and stays that way for the next call.
            std::fs::write(store.root().join("pack.json"), b"{not json").unwrap();
            for _ in 0..2 {
                assert_eq!(get_hips_pack_status(&ctx, "dss-color").unwrap().state, PackState::Idle);
            }
        }

        /// The tests that touch the process-wide slot run in sequence in this one
        /// function (core has no `serial_test`), and it empties the slot at the end.
        #[test]
        fn pack_slot_rules() {
            let (dir, ctx) = test_ctx();
            let ctx = Arc::new(ctx);
            assert!(!pack_is_live());
            assert!(matches!(cancel_hips_pack(), Err(ApiError::NotFound(_))));
            status_idle_without_pack_json();
            status_reports_stored_state_with_counts();

            // A fake live run: no network.
            let cancel = Arc::new(AtomicBool::new(false));
            let latest = Arc::new(Mutex::new(PackProgress::new(sky_only(0), 12)));
            *pack_slot().lock().unwrap() =
                Some(LivePack { survey_id: "dss-color".into(), cancel: cancel.clone(), latest: latest.clone() });

            // A second start conflicts.
            let err = start_hips_pack(ctx.clone(), Arc::new(NullEmitter), sky_only(0)).unwrap_err();
            assert!(matches!(err, ApiError::Conflict(_)), "{err:?}");

            // Status reads the live snapshot; cancel shows as cancelling.
            latest.lock().unwrap().fetched = 3;
            let p = get_hips_pack_status(&ctx, "dss-color").unwrap();
            assert_eq!((p.state, p.fetched), (PackState::Running, 3));
            cancel_hips_pack().unwrap();
            assert!(cancel.load(Ordering::Relaxed));
            assert_eq!(get_hips_pack_status(&ctx, "dss-color").unwrap().state, PackState::Cancelling);

            // Clear is refused while the run is live (Review Focus 4), and the
            // store is left alone.
            let store = HipsStore::new(dir.path(), &DSS_COLOR);
            store.write(0, 0, &jpeg()).unwrap();
            let err = clear_hips_cache(&ctx, "dss-color").unwrap_err();
            assert!(matches!(err, ApiError::Conflict(_)), "{err:?}");
            assert!(store.has(0, 0));

            // A run that emitted its end state no longer blocks a start or a
            // clear, though its thread's guard has not freed the slot yet.
            latest.lock().unwrap().state = PackState::Done;
            assert!(!pack_is_live());
            assert!(while_idle("dss-color", || ()).is_some());

            // The guard of a finished thread empties the slot, and only its own.
            latest.lock().unwrap().state = PackState::Running;
            drop(SlotGuard(Arc::new(AtomicBool::new(false))));
            assert!(pack_is_live(), "a guard for another run must not clear the slot");
            latest.lock().unwrap().state = PackState::Failed;
            assert!(clear_hips_cache(&ctx, "dss-color").unwrap() > 0, "a finished run does not block Clear");
            let leftovers: Vec<_> = std::fs::read_dir(dir.path().join("hips"))
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().contains(".clearing-"))
                .collect();
            assert!(leftovers.is_empty(), "Clear leaves no tombstone behind");
            drop(SlotGuard(cancel));
            assert!(!pack_is_live());
            assert!(matches!(cancel_hips_pack(), Err(ApiError::NotFound(_))));
            *pack_slot().lock().unwrap() = None;
        }
    }
}

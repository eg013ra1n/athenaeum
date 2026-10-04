//! The Solve sub-step's solver seam (spec 2026-10-02 §5.2, plan 6b-2 C14).
//! A project frame whose manifest carries no WCS is plate-solved by the run
//! itself: on the file stage 1 handed on, with hints from its manifest scale
//! and the project's target. Every outcome is cached as a `solve` artifact
//! (group key `""`), keyed on the frame's content, the solver's config and
//! the installed catalog. The run never writes `plate_solves` or `frames`.
//! [`FrameSolver`] is the seam: [`CatalogSolver`] in production, a fake in
//! tests.
//!
//! Concurrency (review M18): [`write_outcome`] is a single UPSERT, so it
//! needs no `BEGIN IMMEDIATE`; one run per subject is enforced upstream, by
//! `start_stacking`'s admission. [`CatalogSolver`] opens the catalog once per
//! run through a `OnceLock`; each concurrent run, like each host, maps its
//! own.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock};

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::collab::frame_meta::ProjectFrameMeta;
use crate::db::stacking::{
    find_artifact, upsert_artifact, NewArtifact, StackSubject, StackingArtifactRow,
};
use crate::fits_writer::{write_fits_f32_with, Card, CardValue, Durability};
use crate::integration::plane_reader::PlaneReader;
use crate::models::Frame;
use crate::plate_solve::config::PlateSolveConfig;
use crate::plate_solve::storage::PlateSolveRecord;
use crate::plate_solve::SolveHints;
use crate::services::ServiceContext;
use crate::stacking::config::stage_hash;

/// One solve the run asks for.
#[derive(Debug, Clone)]
pub struct SolveRequest {
    /// The frame's handle; it names the solve's tracing span (review M12).
    pub handle: i64,
    /// The file to solve: the `calibrated` artifact stage 1 handed on.
    pub path: PathBuf,
    pub hints: SolveHints,
    /// The scale the acceptance gate compares against: the manifest's.
    pub expected_scale_arcsec: Option<f64>,
    pub date_obs: Option<DateTime<Utc>>,
    pub focal_len: Option<f64>,
    pub instrume: Option<String>,
    pub naxis: Option<(i64, i64)>,
    /// Where a unit-domain file's ADU-scaled copy is written for the solve
    /// (plan 6b-2 Task 10c: real calibrated 0..1 frames detect no stars until
    /// scaled). `None` solves the file as it is.
    pub scratch_dir: Option<PathBuf>,
}

impl SolveRequest {
    /// The request for a project frame (spec §5.2):
    /// - the position hint is the project's target;
    /// - the scale hint and the gate's expected scale are the manifest's
    ///   `pixelScaleArcsec`;
    /// - the field of view is that scale times the frame's width, as the
    ///   hosts compute it from the optics (review M11).
    pub fn for_frame(
        handle: i64,
        path: &Path,
        meta: &ProjectFrameMeta,
        target: Option<(f64, f64)>,
    ) -> Self {
        let fov_deg = match (meta.pixel_scale_arcsec, meta.naxis1) {
            (Some(scale), Some(w)) if scale > 0.0 => Some(scale * w as f64 / 3600.0),
            _ => None,
        };
        SolveRequest {
            handle,
            path: path.to_path_buf(),
            hints: SolveHints {
                ra: target.map(|t| t.0),
                dec: target.map(|t| t.1),
                fov_deg,
                rotation: None,
                pixel_scale_arcsec: meta.pixel_scale_arcsec,
            },
            expected_scale_arcsec: meta.pixel_scale_arcsec,
            date_obs: meta
                .date_obs
                .as_deref()
                .and_then(|d| crate::fits_parser::parse_date_obs(d, None).ok()),
            focal_len: meta.focal_len,
            instrume: meta.instrume.clone(),
            naxis: meta.naxis1.zip(meta.naxis2),
            scratch_dir: None,
        }
    }
}

/// What one solve came to. Every outcome is cached, so a frame that failed
/// or was refused is not tried again while its content, the solver config
/// and the catalog hold.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SolveOutcome {
    /// Accepted. The record's `frame_id` is 0, because there is no catalog row.
    Solved(PlateSolveRecord),
    /// The solver found no solution, or could not run.
    Failed(String),
    /// The solver answered, and the acceptance gate refused it.
    Refused(String),
    /// The solve could not be attempted for a reason that says nothing about
    /// the frame (a scratch copy that could not be written). It is never
    /// cached: it cannot be serialized, and [`write_outcome`] refuses it.
    #[serde(skip)]
    Unavailable(String),
}

impl SolveOutcome {
    /// The `outcome` log field.
    pub fn label(&self) -> &'static str {
        match self {
            SolveOutcome::Solved(_) => "solved",
            SolveOutcome::Failed(_) => "failed",
            SolveOutcome::Refused(_) => "refused",
            SolveOutcome::Unavailable(_) => "unavailable",
        }
    }
}

/// A `solve` artifact's payload: the content it is about, the app version
/// that wrote it (review I7; empty for a payload without one), and the
/// outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolveArtifact {
    pub blake3: String,
    #[serde(default)]
    pub app_version: String,
    pub outcome: SolveOutcome,
}

/// The app version a `solve` artifact records: the core crate's, which the
/// release bumps with the app.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Whether a cached `solve` artifact still answers for this frame (plan 6b-2
/// C14, review I7). Its hash must match (same content, solver config and
/// catalog), and it must be a solve, or a failure or refusal written by this
/// very app version. A release may carry a solver that rescues a frame, so a
/// failure from another version is tried once more; a solve is kept.
pub(crate) fn cached_outcome_holds(row: &StackingArtifactRow, hash: &str) -> bool {
    if row.config_hash != hash {
        return false;
    }
    let Some(payload) = row.payload_json.as_deref() else {
        tracing::warn!(frame_id = ?row.frame_id, "solve artifact without a payload; solving again");
        return false;
    };
    match serde_json::from_str::<SolveArtifact>(payload) {
        Ok(artifact) => {
            matches!(artifact.outcome, SolveOutcome::Solved(_))
                || artifact.app_version == APP_VERSION
        }
        Err(e) => {
            tracing::warn!(frame_id = ?row.frame_id, error = %e, "solve artifact unreadable; solving again");
            false
        }
    }
}

/// The `solve` artifact's hash: the frame's content and the solver's
/// identity ([`FrameSolver::identity`]). It is not the Prepare hash, so a
/// debayer toggle never re-solves.
pub fn solve_hash(blake3: &str, solver_identity: &str) -> String {
    stage_hash(
        &serde_json::json!({ "solver": solver_identity }),
        &[&format!("blake3:{blake3}")],
        &[],
    )
}

/// The cached outcome for a project frame's CURRENT content: `None` when
/// there is none, or when the cached one is about other content.
pub(crate) fn stored_outcome(
    conn: &Connection,
    project_id: &str,
    frame_id: i64,
    blake3: &str,
) -> anyhow::Result<Option<SolveOutcome>> {
    let subject = StackSubject::Project(project_id.to_string());
    let Some(row) = find_artifact(conn, &subject, "", "solve", Some(frame_id))? else {
        return Ok(None);
    };
    let Some(payload) = row.payload_json.as_deref() else {
        tracing::warn!(
            project_id,
            frame_id,
            "solve artifact without a payload; treated as no solve"
        );
        return Ok(None);
    };
    match serde_json::from_str::<SolveArtifact>(payload) {
        Ok(artifact) => Ok((artifact.blake3 == blake3).then_some(artifact.outcome)),
        Err(e) => {
            tracing::warn!(project_id, frame_id, error = %e, "solve artifact unreadable; treated as no solve");
            Ok(None)
        }
    }
}

/// Cache one outcome as the frame's `solve` artifact (group key `""`, no
/// file), replacing any earlier one, stamped with this app version.
pub(crate) fn write_outcome(
    conn: &Connection,
    project_id: &str,
    frame_id: i64,
    blake3: &str,
    hash: &str,
    outcome: SolveOutcome,
) -> anyhow::Result<()> {
    if let SolveOutcome::Unavailable(reason) = &outcome {
        anyhow::bail!("an unavailable solve is never cached: {reason}");
    }
    write_artifact(
        conn,
        project_id,
        frame_id,
        hash,
        &SolveArtifact {
            blake3: blake3.to_string(),
            app_version: APP_VERSION.to_string(),
            outcome,
        },
    )
}

/// [`write_outcome`] with the whole payload given (a test writes another
/// app version's outcome through it).
pub(crate) fn write_artifact(
    conn: &Connection,
    project_id: &str,
    frame_id: i64,
    hash: &str,
    artifact: &SolveArtifact,
) -> anyhow::Result<()> {
    let subject = StackSubject::Project(project_id.to_string());
    let payload = serde_json::to_string(artifact)?;
    upsert_artifact(
        conn,
        &NewArtifact {
            subject: &subject,
            frame_id: Some(frame_id),
            group_key: "",
            kind: "solve",
            path: None,
            config_hash: hash,
            size: None,
            modified_at: None,
            payload_json: Some(&payload),
        },
    )?;
    Ok(())
}

/// The solver seam (plan 6b-2 C14).
pub trait FrameSolver: Send + Sync {
    /// The solver's identity for the cache key: its config and the installed
    /// catalog. `Err` when it cannot solve at all (no catalog).
    fn identity(&self) -> Result<String, String>;
    /// Make the solver ready to solve (open the catalog). Called once the run
    /// has a frame that actually needs solving; an `Err` is never cached as a
    /// frame's outcome. Defaults to ready.
    fn ready(&self) -> Result<(), String> {
        Ok(())
    }
    /// Solve one file. Never panics, and never writes the catalog.
    fn solve(&self, request: &SolveRequest, cancel: &AtomicBool) -> SolveOutcome;
}

/// One solve through the real solver with catalog layers already open:
/// `solve_frame_with_hints` (never `solve_frame` or `store_result`), then
/// the acceptance gate at the request's expected scale. A solver panic is
/// caught and reported as a failure, as the hosts' batch does.
pub fn solve_with(
    request: &SolveRequest,
    caches: &solvemyastro::Caches<'_>,
    config: &PlateSolveConfig,
    cancel: &AtomicBool,
) -> SolveOutcome {
    let frame = Frame {
        // Only the solve's tracing span reads it (review M12).
        id: Some(request.handle),
        date_obs: request.date_obs,
        focallen: request.focal_len,
        instrume: request.instrume.clone(),
        naxis1: request.naxis.map(|(w, _)| w as i32),
        naxis2: request.naxis.map(|(_, h)| h as i32),
        ..Frame::default()
    };
    let path = request.path.to_string_lossy().into_owned();
    let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::plate_solve::solve_frame_with_hints(
            &frame,
            &path,
            &request.hints,
            caches,
            config,
            Some(cancel),
        )
    }));
    match attempt {
        Ok(Ok(result)) => {
            match crate::plate_solve::service::acceptance_refusal(
                &result,
                request.expected_scale_arcsec,
                config,
            ) {
                Some(reason) => SolveOutcome::Refused(reason),
                None => SolveOutcome::Solved(crate::plate_solve::service::record_from_result(
                    0, &result,
                )),
            }
        }
        Ok(Err(e)) => SolveOutcome::Failed(format!("{e:#}")),
        Err(_) => {
            tracing::error!(path = %request.path.display(), "the plate solver panicked");
            SolveOutcome::Failed("the plate solver panicked".to_string())
        }
    }
}

/// A float file counts as unit-domain (0..1, calibrated by this app) when the
/// MEDIAN of its finite values is at most this. The median, not the maximum:
/// this app's calibration does not clamp, so a dead master-flat pixel (divided
/// by `FLAT_DENOM_FLOOR`, 2e-5) or a saturated star under a weak flat corner
/// reaches the hundreds in an otherwise 0..1 frame. The two domains differ by
/// about four orders of magnitude, so the median is far from the threshold.
const UNIT_DOMAIN_MEDIAN_MAX: f32 = 1.5;

/// The most values [`is_unit_domain`] sorts.
const DOMAIN_SAMPLE: usize = 65_536;

/// Whether `data` (all planes) is unit-domain: the median of a strided sample
/// of its finite values is at most [`UNIT_DOMAIN_MEDIAN_MAX`].
fn is_unit_domain(data: &[f32]) -> bool {
    let step = (data.len() / DOMAIN_SAMPLE).max(1);
    let mut sample: Vec<f32> = data
        .iter()
        .step_by(step)
        .copied()
        .filter(|v| v.is_finite())
        .collect();
    if sample.is_empty() {
        return false;
    }
    sample.sort_unstable_by(|a, b| a.total_cmp(b));
    sample[sample.len() / 2] <= UNIT_DOMAIN_MEDIAN_MAX
}

/// An ADU-scaled (×65535) float32 copy of a unit-domain FITS, with its
/// copy-through cards. It never touches `src`.
pub fn adu_scaled_copy(src: &Path, dest: &Path) -> anyhow::Result<()> {
    write_scaled(src, dest, false).map(|_| ())
}

/// [`adu_scaled_copy`], but only when `src` is in the unit domain ([`is_unit_domain`]). `Ok(false)` means
/// `src` is already in ADU and nothing was written.
fn adu_copy_if_unit(src: &Path, dest: &Path) -> anyhow::Result<bool> {
    write_scaled(src, dest, true)
}

fn write_scaled(src: &Path, dest: &Path, only_if_unit: bool) -> anyhow::Result<bool> {
    if src == dest {
        anyhow::bail!(
            "the ADU-scaled copy must not overwrite its source {}",
            src.display()
        );
    }
    let unreadable =
        |e: &dyn std::fmt::Display| anyhow::anyhow!("source unreadable: {} ({e})", src.display());
    let reader = PlaneReader::open(src).map_err(|e| unreadable(&e))?;
    let (w, h, c) = (reader.width(), reader.height(), reader.channels());
    let mut data = Vec::with_capacity(w * h * c);
    for plane in 0..c {
        data.extend(reader.read_plane(plane).map_err(|e| unreadable(&e))?);
    }
    if only_if_unit && !is_unit_domain(&data) {
        return Ok(false);
    }
    for v in data.iter_mut() {
        *v *= 65535.0;
    }
    let mut cards = crate::stacking::prepare::source_cards_of(src)
        .map_err(|e| unreadable(&format_args!("{e:#}")))?;
    // The scanner never catalogs a stacking intermediate (as `prepare_cards`).
    cards.retain(|c| c.keyword != "ATH_STK");
    cards.push(
        Card::new("ATH_STK", CardValue::Logical(true))?
            .with_comment("stacking intermediate; never cataloged"),
    );
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("copy unwritable: creating {} ({e})", parent.display()))?;
    }
    write_fits_f32_with(dest, w, h, c, &data, &cards, Durability::Volatile)
        .map_err(|e| anyhow::anyhow!("copy unwritable: writing {} ({e})", dest.display()))?;
    Ok(true)
}

/// Solve a project frame in the right pixel domain (plan 6b-2 Task 10c). The
/// real-data probe found that calibrated 0..1 frames detect no stars until
/// scaled to ADU (0 of 20 solved as prepared, 20 of 20 scaled), so:
/// - a unit-domain file is solved FIRST on an ADU-scaled copy
///   (`scratch/adu_<handle>.fits`), and only a `Failed` falls back to the
///   original; a `Solved` or a `Refused` ends it (a refusal is the acceptance
///   gate's verdict on a found solution, another domain would not fix it);
/// - a float file already in ADU is solved as is, with no copy;
/// - without a `scratch` dir the original is solved, once;
/// - a copy that cannot be written is [`SolveOutcome::Unavailable`], never a
///   cacheable failure, and a partial copy is removed.
///
/// `solve` is the one-file solve (the real solver, or a recording closure).
pub(crate) fn solve_in_domains(
    request: &SolveRequest,
    scratch: Option<&Path>,
    cancel: &AtomicBool,
    solve: &mut dyn FnMut(&SolveRequest) -> SolveOutcome,
) -> SolveOutcome {
    let Some(dir) = scratch else {
        return solve(request);
    };
    let copy = dir.join(format!("adu_{}.fits", request.handle));
    let scaled = match adu_copy_if_unit(&request.path, &copy) {
        Ok(scaled) => scaled,
        Err(e) => {
            let reason = format!("{e:#}");
            tracing::warn!(
                frame_id = request.handle,
                path = %request.path.display(),
                error = %reason,
                "adu-scaled solve copy failed; the frame is not solved and nothing is cached"
            );
            remove_copy(&copy);
            return SolveOutcome::Unavailable(format!(
                "the ADU-scaled solve copy could not be made, {reason}"
            ));
        }
    };
    if !scaled {
        return solve(request);
    }
    let first = solve(&SolveRequest {
        path: copy.clone(),
        ..request.clone()
    });
    remove_copy(&copy);
    let first_reason = match &first {
        SolveOutcome::Failed(reason) => reason.clone(),
        _ => return first,
    };
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return first;
    }
    tracing::warn!(
        frame_id = request.handle,
        error = %first_reason,
        "solve on the adu-scaled copy failed; trying the original pixels"
    );
    match solve(request) {
        SolveOutcome::Failed(second) => {
            SolveOutcome::Failed(format!("{first_reason}; on the unscaled pixels: {second}"))
        }
        other => {
            tracing::debug!(
                frame_id = request.handle,
                outcome = other.label(),
                "solved on the original pixels after the adu-scaled attempt failed"
            );
            other
        }
    }
}

/// Remove a scratch copy. Nothing to do (and nothing to log) when it was never
/// created or its path is blocked; a warning only for a copy that exists and
/// cannot be removed.
fn remove_copy(copy: &Path) {
    if !matches!(copy.try_exists(), Ok(true)) {
        return;
    }
    if let Err(e) = std::fs::remove_file(copy) {
        tracing::warn!(path = %copy.display(), error = %e, "removing the adu-scaled solve copy failed");
    }
}

/// The production solver's identity (plan 6b-3 D14 shares it with the plan):
/// the stored plate-solve config's hash and the installed catalog's stat-only
/// identity under `app_data`. `Err` when no catalog is installed.
pub(crate) fn solver_identity(
    app_data: &Path,
    config: &PlateSolveConfig,
) -> Result<String, String> {
    let catalog = crate::plate_solve::catalog_identity(app_data).map_err(|e| e.to_string())?;
    Ok(format!(
        "{}:{catalog}",
        crate::plate_solve::config::solver_config_hash(config)
    ))
}

/// [`solver_identity`] read from a catalog connection alone (the app-data
/// folder is the database file's own folder, as for [`CatalogSolver`]), so
/// the plan computes the hash the run's cache check compares. `Err` for a
/// database with no file or no installed catalog.
pub(crate) fn installed_solver_identity(conn: &Connection) -> Result<String, String> {
    let db_path = conn
        .path()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| "the database has no file".to_string())?;
    let app_data = Path::new(db_path)
        .parent()
        .ok_or_else(|| "cannot determine the app data directory".to_string())?;
    solver_identity(app_data, &crate::plate_solve::config::load_config(conn))
}

/// The production solver. Its identity (the stored plate-solve config and the
/// installed catalog's stat-only identity) is computed on first use without
/// opening any tier; the catalog layers are mapped lazily, only by the first
/// `solve`, so a run whose frames are all cached never opens them (and an
/// unopenable tier cannot block it).
pub struct CatalogSolver {
    ctx: Arc<ServiceContext>,
    prepared: OnceLock<Result<Prepared, String>>,
    layers: OnceLock<Result<Vec<Arc<solvemyastro::StarCache>>, String>>,
}

struct Prepared {
    app_data: PathBuf,
    config: PlateSolveConfig,
    identity: String,
}

impl CatalogSolver {
    pub fn new(ctx: Arc<ServiceContext>) -> Self {
        CatalogSolver {
            ctx,
            prepared: OnceLock::new(),
            layers: OnceLock::new(),
        }
    }

    fn prepared(&self) -> Result<&Prepared, String> {
        self.prepared
            .get_or_init(|| {
                let db = self
                    .ctx
                    .db
                    .get()
                    .ok_or_else(|| "the database is not initialized".to_string())?;
                let db_path = db.path().to_path_buf();
                let app_data = db_path
                    .parent()
                    .ok_or_else(|| "cannot determine the app data directory".to_string())?
                    .to_path_buf();
                let config = crate::plate_solve::config::load_config(&db.conn());
                let identity = solver_identity(&app_data, &config)?;
                Ok(Prepared {
                    app_data,
                    config,
                    identity,
                })
            })
            .as_ref()
            .map_err(Clone::clone)
    }
}

impl CatalogSolver {
    fn layers_open(&self) -> Result<&Vec<Arc<solvemyastro::StarCache>>, String> {
        let prepared = self.prepared()?;
        self.layers
            .get_or_init(|| {
                crate::plate_solve::open_catalog_layers(&prepared.app_data)
                    .map_err(|e| e.to_string())
            })
            .as_ref()
            .map_err(Clone::clone)
    }
}

impl FrameSolver for CatalogSolver {
    fn identity(&self) -> Result<String, String> {
        self.prepared().map(|p| p.identity.clone())
    }

    fn ready(&self) -> Result<(), String> {
        self.layers_open().map(|_| ())
    }

    fn solve(&self, request: &SolveRequest, cancel: &AtomicBool) -> SolveOutcome {
        let prepared = match self.prepared() {
            Ok(p) => p,
            Err(e) => return SolveOutcome::Failed(e),
        };
        // A safety net: the Solve sub-step calls `ready` first and never gets
        // here after a failure.
        let layers = match self.layers_open() {
            Ok(l) => l,
            Err(e) => return SolveOutcome::Failed(e),
        };
        let refs: Vec<&solvemyastro::StarCache> = layers.iter().map(|l| l.as_ref()).collect();
        let caches = solvemyastro::Caches::layered(&refs);
        solve_in_domains(request, request.scratch_dir.as_deref(), cancel, &mut |r| {
            solve_with(r, &caches, &prepared.config, cancel)
        })
    }
}

/// A solver for tests: it "solves" every file at the fixture target (83.8,
/// −5.4, 1.5″/px) unless told otherwise per file name, and counts its calls.
#[cfg(test)]
pub(crate) struct FakeSolver {
    identity: Result<String, String>,
    by_file: std::collections::HashMap<String, SolveOutcome>,
    calls: std::sync::atomic::AtomicUsize,
    paths: std::sync::Mutex<Vec<PathBuf>>,
    cancels: bool,
    ready_error: Option<String>,
    ready_calls: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl FakeSolver {
    pub(crate) fn solving_all() -> Self {
        FakeSolver {
            identity: Ok("fake:1".to_string()),
            by_file: std::collections::HashMap::new(),
            calls: std::sync::atomic::AtomicUsize::new(0),
            paths: std::sync::Mutex::new(Vec::new()),
            cancels: false,
            ready_error: None,
            ready_calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// A solver whose identity is fine but whose catalog does not open.
    pub(crate) fn not_ready(reason: &str) -> Self {
        FakeSolver {
            ready_error: Some(reason.to_string()),
            ..Self::solving_all()
        }
    }

    pub(crate) fn ready_calls(&self) -> usize {
        self.ready_calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// A solver that raises the run's cancel flag and reports a failure.
    pub(crate) fn cancelling() -> Self {
        FakeSolver {
            cancels: true,
            ..Self::solving_all()
        }
    }

    /// The paths of every request it received, in order.
    pub(crate) fn paths(&self) -> Vec<PathBuf> {
        self.paths.lock().unwrap().clone()
    }

    /// Answer `outcome` for the file named `file_name`.
    pub(crate) fn with(mut self, file_name: &str, outcome: SolveOutcome) -> Self {
        self.by_file.insert(file_name.to_string(), outcome);
        self
    }

    /// A solver that cannot run at all.
    pub(crate) fn unavailable(reason: &str) -> Self {
        FakeSolver {
            identity: Err(reason.to_string()),
            ..Self::solving_all()
        }
    }

    pub(crate) fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The fixture frame's solve: `manifest_wcs_json(64, 48, 83.8, -5.4, 1.5)`.
    pub(crate) fn fixture_record() -> PlateSolveRecord {
        let wcs: crate::collab::frame_meta::ManifestWcs = serde_json::from_value(
            crate::stacking::test_fixtures::manifest_wcs_json(64, 48, 83.8, -5.4, 1.5),
        )
        .expect("the fixture WCS parses");
        wcs.to_record(Some(1.5))
    }
}

#[cfg(test)]
impl FrameSolver for FakeSolver {
    fn identity(&self) -> Result<String, String> {
        self.identity.clone()
    }

    fn ready(&self) -> Result<(), String> {
        self.ready_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.ready_error.clone().map_or(Ok(()), Err)
    }

    fn solve(&self, request: &SolveRequest, cancel: &AtomicBool) -> SolveOutcome {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.paths.lock().unwrap().push(request.path.clone());
        if self.cancels {
            cancel.store(true, std::sync::atomic::Ordering::SeqCst);
            return SolveOutcome::Failed("cancelled".to_string());
        }
        let name = request
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.by_file
            .get(&name)
            .cloned()
            .unwrap_or_else(|| SolveOutcome::Solved(Self::fixture_record()))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Plan 6b-2 C14: the `solve` artifact's payload (the content, the app
    /// version that wrote it, review I7, and the outcome) and its hash over
    /// the frame's content and the solver's identity.
    #[test]
    fn the_solve_artifact_payload_and_hash_are_pinned() {
        let failed = SolveArtifact {
            blake3: "bb".into(),
            app_version: "1.2.3".into(),
            outcome: SolveOutcome::Failed("no match".into()),
        };
        assert_eq!(
            serde_json::to_string(&failed).unwrap(),
            r#"{"blake3":"bb","app_version":"1.2.3","outcome":{"failed":"no match"}}"#
        );
        let refused = SolveArtifact {
            outcome: SolveOutcome::Refused("r".into()),
            ..failed.clone()
        };
        assert_eq!(
            serde_json::to_string(&refused).unwrap(),
            r#"{"blake3":"bb","app_version":"1.2.3","outcome":{"refused":"r"}}"#
        );
        let back: SolveArtifact = serde_json::from_str(
            &serde_json::to_string(&SolveArtifact {
                outcome: SolveOutcome::Solved(FakeSolver::fixture_record()),
                ..failed.clone()
            })
            .unwrap(),
        )
        .unwrap();
        assert!(
            matches!(back.outcome, SolveOutcome::Solved(ref r) if r.crval1 == 83.8 && r.crval2 == -5.4)
        );
        let unversioned: SolveArtifact =
            serde_json::from_str(r#"{"blake3":"bb","outcome":{"failed":"x"}}"#).unwrap();
        assert_eq!(
            unversioned.app_version, "",
            "a payload without a version reads as another version"
        );
        assert_eq!(APP_VERSION, env!("CARGO_PKG_VERSION"));
        let expected = stage_hash(&serde_json::json!({ "solver": "s:c" }), &["blake3:bb"], &[]);
        assert_eq!(solve_hash("bb", "s:c"), expected);
        assert_ne!(solve_hash("bb", "s:d"), expected);
        assert_ne!(solve_hash("cc", "s:c"), expected);
        assert_eq!(
            [
                SolveOutcome::Solved(FakeSolver::fixture_record()).label(),
                SolveOutcome::Failed(String::new()).label(),
                SolveOutcome::Refused(String::new()).label(),
            ],
            ["solved", "failed", "refused"]
        );
    }

    /// Spec §5.2: a project frame's request carries its handle (it names the
    /// solve's span, review M12), the project's target as the position hint,
    /// and the manifest's scale as both the scale hint and the gate's expected
    /// scale. The field of view is that scale times the frame's WIDTH, as the
    /// hosts compute it (review M11).
    #[test]
    fn a_solve_request_carries_the_manifest_scale_and_the_target() {
        let meta = ProjectFrameMeta {
            exptime_sec: 300.0,
            date_obs: Some("2026-01-10T21:00:00".into()),
            naxis1: Some(48),
            naxis2: Some(64),
            pixel_scale_arcsec: Some(1.5),
            bayerpat: None,
            focal_len: Some(400.0),
            aperture: None,
            instrume: Some("cam".into()),
            wcs: None,
        };
        let r = SolveRequest::for_frame(7, Path::new("/w/p.fits"), &meta, Some((83.8, -5.4)));
        assert_eq!(r.handle, 7);
        assert_eq!((r.hints.ra, r.hints.dec), (Some(83.8), Some(-5.4)));
        assert_eq!(r.hints.pixel_scale_arcsec, Some(1.5));
        assert_eq!(
            r.hints.fov_deg,
            Some(1.5 * 48.0 / 3600.0),
            "the width, not the larger side"
        );
        assert_eq!(r.expected_scale_arcsec, Some(1.5));
        assert_eq!(
            r.date_obs.map(|d| d.to_rfc3339()).as_deref(),
            Some("2026-01-10T21:00:00+00:00")
        );
        assert_eq!((r.focal_len, r.naxis), (Some(400.0), Some((48, 64))));
        let bare = SolveRequest::for_frame(
            7,
            Path::new("/w/p.fits"),
            &ProjectFrameMeta {
                pixel_scale_arcsec: None,
                ..meta
            },
            None,
        );
        assert_eq!(
            (
                bare.hints.ra,
                bare.hints.fov_deg,
                bare.expected_scale_arcsec
            ),
            (None, None, None)
        );
    }

    /// Review M17: without an installed catalog the production solver says
    /// so: `identity` fails, and `solve` reports the same reason without
    /// touching the file.
    #[test]
    fn the_catalog_solver_without_a_catalog_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        let solver = CatalogSolver::new(Arc::new(ServiceContext::new_for_tests(
            tmp.path().join("catalog.db"),
        )));
        let err = solver.identity().unwrap_err();
        assert!(err.contains("not installed"), "{err}");
        let request = SolveRequest::for_frame(
            1,
            Path::new("/nowhere/p.fits"),
            &ProjectFrameMeta::default(),
            None,
        );
        assert!(
            matches!(solver.solve(&request, &AtomicBool::new(false)), SolveOutcome::Failed(ref e) if *e == err)
        );
    }

    /// The ADU-scaled copy multiplies every value by 65535, carries the
    /// `ATH_STK` marker so the scanner never catalogs it, and refuses to
    /// overwrite its source.
    #[test]
    fn the_adu_scaled_copy_scales_marks_and_refuses_its_source() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src.fits");
        let data: Vec<f32> = (0..12).map(|i| i as f32 / 16.0).collect();
        crate::fits_writer::write_fits_f32(&src, 4, 3, 1, &data, &[]).unwrap();
        let before = std::fs::read(&src).unwrap();
        let dest = tmp.path().join("out").join("copy.fits");
        adu_scaled_copy(&src, &dest).unwrap();
        let reader = PlaneReader::open(&dest).unwrap();
        let plane = reader.read_plane(0).unwrap();
        assert_eq!(plane.len(), 12);
        for (got, want) in plane.iter().zip(&data) {
            assert!((got - want * 65535.0).abs() < 0.01, "{got} vs {want}");
        }
        let bytes = std::fs::read(&dest).unwrap();
        let header = String::from_utf8_lossy(&bytes[..2880]);
        assert!(header.contains("ATH_STK = "), "{header}");
        assert!(adu_scaled_copy(&src, &src).is_err());
        assert_eq!(
            std::fs::read(&src).unwrap(),
            before,
            "the source is untouched"
        );
    }

    fn describe(o: &SolveOutcome) -> String {
        match o {
            SolveOutcome::Solved(r) => format!(
                "solved {:.3}\"/px at ({:.4}, {:.4}), {} matched",
                r.pixel_scale_arcsec, r.crval1, r.crval2, r.matched_stars
            ),
            SolveOutcome::Failed(e) => format!("failed: {e}"),
            SolveOutcome::Refused(e) => format!("refused: {e}"),
            SolveOutcome::Unavailable(e) => format!("unavailable: {e}"),
        }
    }

    /// One kind's counts in the probe: mono, or OSC by the manifest's `bayerpat`.
    #[derive(Default)]
    struct Tally {
        tried: usize,
        /// Solved by the first attempt (the ADU-scaled copy of a unit-domain
        /// file, or the file itself when it is already in ADU).
        first_ok: usize,
        /// Solved only by the fallback (the original pixels).
        fallback_ok: usize,
    }

    /// Plan 6b-2 C14, "real data first": the real solver and the real catalog
    /// on the landed project replicas of a COPY of the `.dev` catalog. These
    /// files are calibrated to 0..1. The probe runs the very function the
    /// solver ships ([`solve_in_domains`]) and reports per frame and in total
    /// how many solved on the first attempt and how many needed the fallback.
    /// - The sample is stratified (review I8): up to `limit / 2` frames with a
    ///   manifest `bayerpat` (OSC, debayered by Prepare into a three-plane file)
    ///   and up to `limit / 2` without; the summary prints both sub-rates.
    /// - A probe that solves nothing fails (review M16).
    /// - It is read-only on the catalog copy and on every landed file; prepared
    ///   and scaled copies go to a temp dir.
    #[test]
    #[ignore = "needs ATHENAEUM_SOLVE_PROBE_DB (a copy of the .dev catalog) and ATHENAEUM_SOLVE_PROBE_APP_DATA (the .dev app-data dir, for its star catalog)"]
    fn real_data_solve_probe() {
        use crate::stacking::prepare::{
            execute_prepare, prepare_action, probe_file, PrepareAction, PrepareJob,
        };
        let started = std::time::Instant::now();
        let db = std::env::var("ATHENAEUM_SOLVE_PROBE_DB").expect("ATHENAEUM_SOLVE_PROBE_DB");
        let app_data = std::env::var("ATHENAEUM_SOLVE_PROBE_APP_DATA")
            .expect("ATHENAEUM_SOLVE_PROBE_APP_DATA");
        let limit: usize = std::env::var("ATHENAEUM_SOLVE_PROBE_LIMIT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(20);
        let conn =
            Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let config = crate::plate_solve::config::load_config(&conn);
        let layers = crate::plate_solve::open_catalog_layers(Path::new(&app_data))
            .expect("the .dev star catalog");
        let refs: Vec<&solvemyastro::StarCache> = layers.iter().map(|l| l.as_ref()).collect();
        let caches = solvemyastro::Caches::layered(&refs);
        let scratch = tempfile::tempdir().unwrap();
        let go = AtomicBool::new(false);
        let rows: Vec<(String, String, String, String)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT project_id, frame_uuid, landed_path, manifest_json FROM project_frames_local
                     WHERE landed_path IS NOT NULL ORDER BY project_id, frame_uuid",
                )
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        let (mut osc_rows, mut mono_rows) = (Vec::new(), Vec::new());
        let mut unreadable = 0usize;
        for (project_id, uuid, landed, manifest_json) in rows {
            match crate::collab::frame_meta::parse_project_frame_meta(&manifest_json) {
                Ok(meta) if meta.bayerpat.is_some() => {
                    osc_rows.push((project_id, uuid, landed, meta))
                }
                Ok(meta) => mono_rows.push((project_id, uuid, landed, meta)),
                Err(e) => {
                    unreadable += 1;
                    println!("{uuid}: manifest unreadable: {e:#}");
                }
            }
        }
        let half = limit / 2;
        let sample: Vec<(bool, (String, String, String, ProjectFrameMeta))> = osc_rows
            .into_iter()
            .take(half)
            .map(|r| (true, r))
            .chain(mono_rows.into_iter().take(half).map(|r| (false, r)))
            .collect();
        let (mut mono, mut osc) = (Tally::default(), Tally::default());
        let (mut unsupported, mut absent, mut manifest_wcs) = (0usize, 0usize, 0usize);
        for (handle, (is_osc, (project_id, uuid, landed, meta))) in sample.into_iter().enumerate() {
            let tally = if is_osc { &mut osc } else { &mut mono };
            if meta.wcs.is_some() {
                manifest_wcs += 1;
            }
            let target = match crate::db::collab::get_project(&conn, &project_id) {
                Ok(p) => p.map(|p| (p.target_ra_deg, p.target_dec_deg)),
                Err(e) => {
                    println!("{uuid}: project {project_id} unreadable: {e:#}");
                    None
                }
            };
            let landed = PathBuf::from(landed);
            if !landed.exists() {
                absent += 1;
                println!(
                    "{uuid}: not on disk at {} (is its volume mounted?)",
                    landed.display()
                );
                continue;
            }
            let shape = probe_file(&landed);
            let action = prepare_action(&shape, true);
            let path = match &action {
                PrepareAction::Unsupported(why) => {
                    unsupported += 1;
                    println!("{uuid}: unsupported file: {why}");
                    continue;
                }
                PrepareAction::InPlace => landed.clone(),
                PrepareAction::Write { .. } => {
                    let job = PrepareJob {
                        src: landed.clone(),
                        shape: shape.clone(),
                        action: action.clone(),
                        converted_out: scratch.path().join(format!("p_{uuid}.fits")),
                        debayered_out: scratch.path().join(format!("p_{uuid}_d.fits")),
                        durability: Durability::Volatile,
                    };
                    execute_prepare(&job, None, &go)
                        .expect("prepare")
                        .calibrated
                }
            };
            tally.tried += 1;
            // Handles are 1-based here: the scratch copy is named by it.
            let request = SolveRequest::for_frame(handle as i64 + 1, &path, &meta, target);
            let mut attempts: Vec<String> = Vec::new();
            let outcome = solve_in_domains(&request, Some(scratch.path()), &go, &mut |r| {
                let o = solve_with(r, &caches, &config, &go);
                let which = if r.path == path {
                    "original"
                } else {
                    "adu-scaled"
                };
                attempts.push(format!("{which}: {}", describe(&o)));
                o
            });
            if matches!(outcome, SolveOutcome::Solved(_)) {
                if attempts.len() == 1 {
                    tally.first_ok += 1;
                } else {
                    tally.fallback_ok += 1;
                }
            }
            println!(
                "{uuid} ({}, {}): {}",
                if is_osc { "osc" } else { "mono" },
                if meta.wcs.is_some() {
                    "manifest wcs"
                } else {
                    "no manifest wcs"
                },
                attempts.join(" | ")
            );
        }
        println!(
            "solve probe: mono {}/{} solved on the first attempt, {} needed the fallback; osc {}/{} solved on the first attempt, {} needed the fallback; unsupported {unsupported}, not on disk {absent}, unreadable manifests {unreadable}, with a manifest wcs {manifest_wcs}; wall {:.1}s",
            mono.first_ok,
            mono.tried,
            mono.fallback_ok,
            osc.first_ok,
            osc.tried,
            osc.fallback_ok,
            started.elapsed().as_secs_f64()
        );
        assert!(
            mono.tried + osc.tried > 0,
            "no landed project frame in the catalog copy"
        );
        // Review M16: a probe that solved nothing is a red run, not a line to read.
        assert!(
            mono.first_ok + mono.fallback_ok + osc.first_ok + osc.fallback_ok > 0,
            "no frame solved"
        );
    }

    /// Spec §15.3 "WCS": an independent plate solve of a grid master agrees
    /// with the grid's WCS written on it within 0.5 px. Run by the 6b-3
    /// acceptance agent on a real project master.
    #[test]
    #[ignore = "needs ATHENAEUM_GRID_PROBE_MASTER (a grid master FITS) and ATHENAEUM_SOLVE_PROBE_APP_DATA (the .dev app-data dir, for its star catalog)"]
    fn real_data_grid_master_wcs_probe() {
        let master = PathBuf::from(
            std::env::var("ATHENAEUM_GRID_PROBE_MASTER").expect("ATHENAEUM_GRID_PROBE_MASTER"),
        );
        let app_data = std::env::var("ATHENAEUM_SOLVE_PROBE_APP_DATA")
            .expect("ATHENAEUM_SOLVE_PROBE_APP_DATA");
        let header = crate::fits_parser::FitsHeader::from_path(&master).unwrap();
        let f = |k: &str| header.get_f64(k).unwrap_or_else(|| panic!("{k} missing"));
        let written = solvemyastro::wcs::WcsSolution {
            crpix: (f("CRPIX1") - 1.0, f("CRPIX2") - 1.0),
            crval: (f("CRVAL1"), f("CRVAL2")),
            cd: [[f("CD1_1"), f("CD1_2")], [f("CD2_1"), f("CD2_2")]],
            sip_forward: None,
            sip_reverse: None,
        };
        let (w, h) = {
            let r = PlaneReader::open(&master).unwrap();
            (r.width(), r.height())
        };
        let scale = written.pixel_scale_arcsec();
        let centre = written.pixel_to_sky((w as f64 - 1.0) / 2.0, (h as f64 - 1.0) / 2.0);
        let layers = crate::plate_solve::open_catalog_layers(Path::new(&app_data))
            .expect("the .dev star catalog");
        let refs: Vec<&solvemyastro::StarCache> = layers.iter().map(|l| l.as_ref()).collect();
        let caches = solvemyastro::Caches::layered(&refs);
        let config = PlateSolveConfig::default();
        let scratch = tempfile::tempdir().unwrap();
        let go = AtomicBool::new(false);
        let request = SolveRequest {
            handle: 0,
            path: master.clone(),
            hints: SolveHints {
                ra: Some(centre.0),
                dec: Some(centre.1),
                fov_deg: Some(scale * w as f64 / 3600.0),
                rotation: None,
                pixel_scale_arcsec: Some(scale),
            },
            expected_scale_arcsec: Some(scale),
            date_obs: None,
            focal_len: None,
            instrume: None,
            naxis: Some((w as _, h as _)),
            scratch_dir: Some(scratch.path().to_path_buf()),
        };
        let outcome = solve_in_domains(&request, Some(scratch.path()), &go, &mut |r| {
            solve_with(r, &caches, &config, &go)
        });
        let SolveOutcome::Solved(record) = outcome else {
            panic!("the grid master did not solve: {outcome:?}");
        };
        let solved = record.to_solution().unwrap();
        let mut worst = 0f64;
        for i in 1..10 {
            for j in 1..10 {
                let (x, y) = (i as f64 * w as f64 / 10.0, j as f64 * h as f64 / 10.0);
                let (ra, dec) = written.pixel_to_sky(x, y);
                let (sx, sy) = solved.sky_to_pixel(ra, dec);
                worst = worst.max((sx - x).hypot(sy - y));
            }
        }
        println!(
            "grid master wcs probe: {}: worst distance written vs solved {worst:.3} px",
            master.display()
        );
        assert!(
            worst <= 0.5,
            "the grid WCS disagrees with an independent solve by {worst} px"
        );
    }

    // ---- solve_in_domains (plan 6b-2 Task 10c, review fix round 1) ----

    fn plain_request(handle: i64, path: &Path) -> SolveRequest {
        SolveRequest {
            handle,
            path: path.to_path_buf(),
            hints: SolveHints::default(),
            expected_scale_arcsec: None,
            date_obs: None,
            focal_len: None,
            instrume: None,
            naxis: None,
            scratch_dir: None,
        }
    }

    fn write_plane(path: &Path, values: &[f32]) {
        crate::fits_writer::write_fits_f32(path, 4, 2, 1, values, &[]).unwrap();
    }

    fn read_plane0(path: &Path) -> Vec<f32> {
        PlaneReader::open(path).unwrap().read_plane(0).unwrap()
    }

    fn solved() -> SolveOutcome {
        SolveOutcome::Solved(FakeSolver::fixture_record())
    }

    /// Everything `warn!` and below writes while `f` runs on this thread.
    pub(crate) fn captured_logs(f: impl FnOnce()) -> String {
        #[derive(Clone)]
        struct Buf(Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Buf {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Buf {
            type Writer = Buf;
            fn make_writer(&'a self) -> Buf {
                self.clone()
            }
        }
        let buf = Buf(Arc::new(std::sync::Mutex::new(Vec::new())));
        let subscriber = tracing_subscriber::fmt()
            .with_writer(buf.clone())
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, f);
        let bytes = buf.0.lock().unwrap().clone();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// (a) A unit-domain file is solved first on `adu_<handle>.fits`, whose
    /// pixels are the source times 65535; the original is the fallback and
    /// the copy is gone afterwards. (b) A first `Solved` is one call.
    #[test]
    fn a_unit_domain_file_is_solved_scaled_first_then_as_is() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("c_light.fits");
        let values = [0.0f32, 0.25, 0.5, 1.0, 0.1, 0.2, 0.3, 0.4];
        write_plane(&src, &values);
        let scratch = dir.path().join("tmp");
        let go = AtomicBool::new(false);

        let mut calls: Vec<PathBuf> = Vec::new();
        let mut seen_pixels: Vec<f32> = Vec::new();
        let mut existed = false;
        let outcome = solve_in_domains(&plain_request(7, &src), Some(&scratch), &go, &mut |r| {
            calls.push(r.path.clone());
            if calls.len() == 1 {
                existed = r.path.exists();
                seen_pixels = read_plane0(&r.path);
                SolveOutcome::Failed("no stars".into())
            } else {
                solved()
            }
        });
        assert!(matches!(outcome, SolveOutcome::Solved(_)));
        assert_eq!(calls, vec![scratch.join("adu_7.fits"), src.clone()]);
        assert!(existed, "the copy exists during the first call");
        let want: Vec<f32> = values.iter().map(|v| v * 65535.0).collect();
        assert_eq!(seen_pixels, want);
        assert!(!scratch.join("adu_7.fits").exists(), "the copy is removed");
        assert_eq!(
            read_plane0(&src),
            values.to_vec(),
            "the source is untouched"
        );

        // (b) a first success is the only call.
        let mut n = 0;
        let outcome = solve_in_domains(&plain_request(7, &src), Some(&scratch), &go, &mut |_| {
            n += 1;
            solved()
        });
        assert!(matches!(outcome, SolveOutcome::Solved(_)));
        assert_eq!(n, 1);
        assert!(!scratch.join("adu_7.fits").exists());
    }

    /// Both attempts failing reports both reasons; the first is logged.
    #[test]
    fn both_failed_attempts_are_reported_and_the_first_is_logged() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("c.fits");
        write_plane(&src, &[0.5; 8]);
        let go = AtomicBool::new(false);
        let mut n = 0;
        let mut outcome = None;
        let logs = captured_logs(|| {
            outcome = Some(solve_in_domains(
                &plain_request(3, &src),
                Some(&dir.path().join("tmp")),
                &go,
                &mut |_| {
                    n += 1;
                    SolveOutcome::Failed(format!("reason {n}"))
                },
            ));
        });
        match outcome.unwrap() {
            SolveOutcome::Failed(text) => {
                assert!(
                    text.contains("reason 1") && text.contains("reason 2"),
                    "{text}"
                )
            }
            other => panic!("{other:?}"),
        }
        assert!(logs.contains("WARN") && logs.contains("reason 1"), "{logs}");
    }

    /// (c) A cancel raised inside the first call stops the fallback.
    #[test]
    fn a_cancel_inside_the_first_call_skips_the_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("c.fits");
        write_plane(&src, &[0.5; 8]);
        let go = AtomicBool::new(false);
        let mut n = 0;
        let outcome = solve_in_domains(
            &plain_request(1, &src),
            Some(&dir.path().join("tmp")),
            &go,
            &mut |_| {
                n += 1;
                go.store(true, std::sync::atomic::Ordering::Relaxed);
                SolveOutcome::Failed("cancelled".into())
            },
        );
        assert_eq!(n, 1);
        assert!(matches!(outcome, SolveOutcome::Failed(_)));
    }

    /// (d) Without a scratch dir the original is solved, once.
    #[test]
    fn without_a_scratch_dir_the_original_is_solved_once() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("c.fits");
        write_plane(&src, &[0.5; 8]);
        let go = AtomicBool::new(false);
        let mut calls = Vec::new();
        let outcome = solve_in_domains(&plain_request(1, &src), None, &go, &mut |r| {
            calls.push(r.path.clone());
            SolveOutcome::Failed("x".into())
        });
        assert_eq!(calls, vec![src]);
        assert!(matches!(outcome, SolveOutcome::Failed(ref t) if t == "x"));
    }

    /// (e) A scratch path that cannot be created is not a cacheable failure:
    /// it is `Unavailable`, it is logged, no solve ran, and `write_outcome`
    /// refuses it.
    #[test]
    fn an_unwritable_scratch_is_unavailable_and_never_cached() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("c.fits");
        write_plane(&src, &[0.5; 8]);
        let blocker = dir.path().join("tmp");
        std::fs::write(&blocker, b"a file, not a directory").unwrap();
        let go = AtomicBool::new(false);
        let mut n = 0;
        let mut outcome = None;
        let logs = captured_logs(|| {
            outcome = Some(solve_in_domains(
                &plain_request(1, &src),
                Some(&blocker),
                &go,
                &mut |_| {
                    n += 1;
                    solved()
                },
            ));
        });
        let outcome = outcome.unwrap();
        assert_eq!(n, 0);
        assert!(
            matches!(outcome, SolveOutcome::Unavailable(_)),
            "{outcome:?}"
        );
        assert!(
            logs.contains("WARN") && logs.contains("adu-scaled solve copy failed"),
            "{logs}"
        );
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        crate::db::stacking::insert_test_project(&conn, "p1");
        assert!(write_outcome(&conn, "p1", 1, "b", "h", outcome.clone()).is_err());
        assert!(serde_json::to_string(&outcome).is_err());
    }

    /// (f) A float file already in ADU is solved as is, with no copy.
    #[test]
    fn an_adu_domain_float_file_is_solved_as_is_without_a_copy() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("c.fits");
        write_plane(&src, &[0.0, 1000.0, 20000.0, 65535.0, 5.0, 6.0, 7.0, 8.0]);
        let scratch = dir.path().join("tmp");
        let go = AtomicBool::new(false);
        let mut calls = Vec::new();
        let outcome = solve_in_domains(&plain_request(1, &src), Some(&scratch), &go, &mut |r| {
            calls.push(r.path.clone());
            SolveOutcome::Failed("x".into())
        });
        assert_eq!(calls, vec![src]);
        assert!(matches!(outcome, SolveOutcome::Failed(_)));
        assert!(!scratch.join("adu_1.fits").exists());
    }

    /// (g) A first `Refused` is final: no fallback.
    #[test]
    fn a_refused_first_attempt_has_no_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("c.fits");
        write_plane(&src, &[0.5; 8]);
        let go = AtomicBool::new(false);
        let mut n = 0;
        let outcome = solve_in_domains(
            &plain_request(1, &src),
            Some(&dir.path().join("tmp")),
            &go,
            &mut |_| {
                n += 1;
                SolveOutcome::Refused("wrong scale".into())
            },
        );
        assert_eq!(n, 1);
        assert!(matches!(outcome, SolveOutcome::Refused(_)));
    }

    /// Review round 2: one dead-flat pixel at 400 and a few at 2.0 in a 0..1
    /// frame do not make it an ADU frame (median, not max), and an unreadable
    /// source / blocked scratch warn once, not twice.
    #[test]
    fn a_unit_frame_with_outlier_pixels_is_still_solved_scaled_first() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("c.fits");
        let mut values = vec![0.4f32; 16 * 16];
        values[0] = 400.0;
        values[1] = 2.0;
        values[2] = 2.0;
        values[3] = 2.0;
        crate::fits_writer::write_fits_f32(&src, 16, 16, 1, &values, &[]).unwrap();
        let scratch = dir.path().join("tmp");
        let go = AtomicBool::new(false);
        let mut calls = Vec::new();
        solve_in_domains(&plain_request(5, &src), Some(&scratch), &go, &mut |r| {
            calls.push(r.path.clone());
            solved()
        });
        assert_eq!(calls, vec![scratch.join("adu_5.fits")]);
        assert!(is_unit_domain(&values));
        assert!(!is_unit_domain(&[900.0, 1000.0, 20000.0]));
    }

    #[test]
    fn a_blocked_scratch_logs_one_warning_and_names_the_copy() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("c.fits");
        write_plane(&src, &[0.5; 8]);
        let blocker = dir.path().join("tmp");
        std::fs::write(&blocker, b"x").unwrap();
        let go = AtomicBool::new(false);
        let mut outcome = None;
        let logs = captured_logs(|| {
            outcome = Some(solve_in_domains(
                &plain_request(1, &src),
                Some(&blocker),
                &go,
                &mut |_| solved(),
            ));
        });
        assert_eq!(logs.matches("WARN").count(), 1, "{logs}");
        match outcome.unwrap() {
            SolveOutcome::Unavailable(t) => assert!(t.contains("copy unwritable"), "{t}"),
            other => panic!("{other:?}"),
        }
        let missing = dir.path().join("missing.fits");
        let outcome = solve_in_domains(
            &plain_request(1, &missing),
            Some(&dir.path().join("t2")),
            &go,
            &mut |_| solved(),
        );
        match outcome {
            SolveOutcome::Unavailable(t) => assert!(t.contains("source unreadable"), "{t}"),
            other => panic!("{other:?}"),
        }
    }

    /// Plan 6b-3 D14: the plan reads the solver's identity from the catalog
    /// connection alone, and it is the identity the run's solver reports —
    /// so the plan and the run agree on which cached failures stand. A
    /// database with no file has none.
    // `use super::*` brings the `FrameSolver` trait in for `identity()`
    // (review M15): keep the import if this test moves.
    #[test]
    fn the_installed_identity_is_the_runs_identity() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let catalog = crate::plate_solve::catalog_root(dir.path());
        std::fs::create_dir_all(&catalog).unwrap();
        std::fs::write(catalog.join("stars.smac"), b"").unwrap();
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let installed = installed_solver_identity(&conn).expect("a catalog is installed");
        assert_eq!(CatalogSolver::new(ctx).identity().unwrap(), installed);
        let memory = rusqlite::Connection::open_in_memory().unwrap();
        assert!(installed_solver_identity(&memory).is_err());
    }
}

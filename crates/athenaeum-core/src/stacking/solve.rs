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

use anyhow::Context;
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::collab::frame_meta::ProjectFrameMeta;
use crate::db::stacking::{
    find_artifact, upsert_artifact, NewArtifact, StackSubject, StackingArtifactRow,
};
use crate::fits_writer::{write_fits_f32_with, Durability};
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
}

impl SolveOutcome {
    /// The `outcome` log field.
    pub fn label(&self) -> &'static str {
        match self {
            SolveOutcome::Solved(_) => "solved",
            SolveOutcome::Failed(_) => "failed",
            SolveOutcome::Refused(_) => "refused",
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

/// An ADU-scaled (×65535) float32 copy of a unit-domain FITS, with its
/// copy-through cards. It is the real-data probe's comparison input, and the
/// fallback a probe result may call for (plan 6b-2 Task 10c Step 8). It
/// never touches `src`.
pub fn adu_scaled_copy(src: &Path, dest: &Path) -> anyhow::Result<()> {
    let reader = PlaneReader::open(src).with_context(|| format!("opening {}", src.display()))?;
    let (w, h, c) = (reader.width(), reader.height(), reader.channels());
    let mut data = Vec::with_capacity(w * h * c);
    for plane in 0..c {
        let values = reader
            .read_plane(plane)
            .with_context(|| format!("reading {}", src.display()))?;
        data.extend(values.into_iter().map(|v| v * 65535.0));
    }
    let cards = crate::stacking::prepare::source_cards_of(src)?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_fits_f32_with(dest, w, h, c, &data, &cards, Durability::Volatile)
        .map_err(|e| anyhow::anyhow!("writing {}: {e}", dest.display()))?;
    Ok(())
}

/// The production solver. It uses the installed catalog, opened on first use
/// (a run that solves nothing never opens it), and the stored plate-solve
/// config.
pub struct CatalogSolver {
    ctx: Arc<ServiceContext>,
    opened: OnceLock<Result<OpenedCatalog, String>>,
}

struct OpenedCatalog {
    layers: Vec<Arc<solvemyastro::StarCache>>,
    config: PlateSolveConfig,
    identity: String,
}

impl CatalogSolver {
    pub fn new(ctx: Arc<ServiceContext>) -> Self {
        CatalogSolver {
            ctx,
            opened: OnceLock::new(),
        }
    }

    fn opened(&self) -> Result<&OpenedCatalog, String> {
        self.opened
            .get_or_init(|| {
                let db = self
                    .ctx
                    .db
                    .get()
                    .ok_or_else(|| "the database is not initialized".to_string())?;
                let db_path = db.path().to_path_buf();
                let app_data = db_path
                    .parent()
                    .ok_or_else(|| "cannot determine the app data directory".to_string())?;
                let config = crate::plate_solve::config::load_config(&db.conn());
                let layers =
                    crate::plate_solve::open_catalog_layers(app_data).map_err(|e| e.to_string())?;
                let catalog =
                    crate::plate_solve::catalog_identity(app_data).map_err(|e| e.to_string())?;
                let identity = format!(
                    "{}:{catalog}",
                    crate::plate_solve::config::solver_config_hash(&config)
                );
                Ok(OpenedCatalog {
                    layers,
                    config,
                    identity,
                })
            })
            .as_ref()
            .map_err(Clone::clone)
    }
}

impl FrameSolver for CatalogSolver {
    fn identity(&self) -> Result<String, String> {
        self.opened().map(|o| o.identity.clone())
    }

    fn solve(&self, request: &SolveRequest, cancel: &AtomicBool) -> SolveOutcome {
        let opened = match self.opened() {
            Ok(o) => o,
            Err(e) => return SolveOutcome::Failed(e),
        };
        let refs: Vec<&solvemyastro::StarCache> =
            opened.layers.iter().map(|l| l.as_ref()).collect();
        let caches = solvemyastro::Caches::layered(&refs);
        solve_with(request, &caches, &opened.config, cancel)
    }
}

/// A solver for tests: it "solves" every file at the fixture target (83.8,
/// −5.4, 1.5″/px) unless told otherwise per file name, and counts its calls.
#[cfg(test)]
pub(crate) struct FakeSolver {
    identity: Result<String, String>,
    by_file: std::collections::HashMap<String, SolveOutcome>,
    calls: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl FakeSolver {
    pub(crate) fn solving_all() -> Self {
        FakeSolver {
            identity: Ok("fake:1".to_string()),
            by_file: std::collections::HashMap::new(),
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
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

    fn solve(&self, request: &SolveRequest, _cancel: &AtomicBool) -> SolveOutcome {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
mod tests {
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
}

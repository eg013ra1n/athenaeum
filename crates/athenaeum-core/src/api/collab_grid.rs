//! The project's canonical grid at the command layer (spec 2026-10-02 §6,
//! §9.1, plan 6b-2 C16): the proposal, the coordinator's save, and the
//! `collab-grid-changed` event. The saving device emits it from the save
//! command; every other device's live feed emits it when the hub's grid moves.

use rusqlite::Connection;

use crate::account::AccountClientError;
use crate::api::collab::{client_err, has_cap, internal};
use crate::api::{db, ApiError};
use crate::collab::grid::CanonicalGrid;
use crate::collab::grid_geometry::{
    count_inside, grid_input_of, propose_grid, GridInput, GridTarget,
};
use crate::collab::hub_client::CollabClient;
use crate::events::{emit_event, ProgressEmitter};
use crate::services::ServiceContext;

/// Emitted by a successful `set_collab_project_grid` on the saving device,
/// and by every live-feed refresh that moved a project's cached grid.
pub const COLLAB_GRID_CHANGED_EVENT: &str = "collab-grid-changed";

/// Payload of [`COLLAB_GRID_CHANGED_EVENT`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CollabGridChanged {
    pub project_id: String,
}

/// What `get_collab_grid_proposal` answers (spec §9.1): the confirmed
/// grid, the proposal from the whole manifest, how many accepted frames with
/// a WCS lie at least half inside the proposal, and whether this account may
/// save a grid. `proposal` is `None` when no accepted frame carries a pixel
/// scale, and also when the median scale rounds outside the hub's `(0, 100]`
/// (review M8).
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct GridProposal {
    pub current: Option<CanonicalGrid>,
    pub proposal: Option<CanonicalGrid>,
    pub frames_inside: usize,
    pub can_edit: bool,
}

pub const NO_GRID_CAP_MSG: &str =
    "This account cannot edit the project's grid (it needs project.edit and membership).";

/// The cached confirmed grid, parsed.
pub fn stored_grid(conn: &Connection, project_id: &str) -> Result<Option<CanonicalGrid>, ApiError> {
    match crate::db::collab::canonical_grid(conn, project_id).map_err(internal)? {
        None => Ok(None),
        Some(json) => serde_json::from_str(&json).map(Some).map_err(|e| {
            tracing::error!(project_id, error = %e, "cached canonical grid is unreadable");
            ApiError::Internal(format!(
                "cached canonical grid of {project_id} is unreadable: {e}"
            ))
        }),
    }
}

/// The proposal's inputs: every published, accepted frame of the manifest
/// mirror (spec §6.2 — the whole manifest, so every device proposes the same
/// grid for one manifest version), in `frame_uuid` order. A row whose
/// manifest does not parse is logged and left out.
pub fn grid_inputs_of_project(
    conn: &Connection,
    project_id: &str,
) -> Result<Vec<GridInput>, ApiError> {
    let rows = crate::db::collab_frames::list_for_project(conn, project_id).map_err(internal)?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows
        .iter()
        .filter(|r| crate::db::collab_frames::part_of_project(r))
    {
        match crate::collab::frame_meta::parse_project_frame_meta(&row.manifest_json) {
            Ok(meta) => out.push(grid_input_of(&meta)),
            Err(e) => {
                tracing::warn!(project_id, frame_uuid = %row.frame_uuid, error = %format!("{e:#}"), "project frame metadata unreadable; left out of the grid proposal")
            }
        }
    }
    Ok(out)
}

/// `get_collab_grid_proposal` (spec §6.2, §9.1). DB-only.
pub fn get_collab_grid_proposal(
    ctx: &ServiceContext,
    project_id: &str,
) -> Result<GridProposal, ApiError> {
    let database = db(ctx)?;
    let conn = database.conn();
    let project = crate::api::collab_exchange::live_project(&conn, project_id)?;
    let can_edit = has_cap(
        project.is_coordinator,
        &project.gov_caps_json,
        "project.edit",
    );
    let current = stored_grid(&conn, project_id)?;
    let inputs = grid_inputs_of_project(&conn, project_id)?;
    let target = GridTarget {
        ra_deg: project.target_ra_deg,
        dec_deg: project.target_dec_deg,
        radius_deg: project.target_radius_deg,
    };
    let proposal = propose_grid(&inputs, &target);
    let frames_inside = proposal
        .as_ref()
        .map(|g| count_inside(&inputs, g))
        .unwrap_or(0);
    tracing::debug!(
        project_id,
        frames_inside,
        count = inputs.len(),
        "grid proposed"
    );
    Ok(GridProposal {
        current,
        proposal,
        frames_inside,
        can_edit,
    })
}

/// `set_collab_project_grid` (spec §6.3): normalize, validate (the hub's
/// bounds), refuse a cached card without `project.edit` before any request
/// (the hub's 403 stays the backstop), `PUT /projects/{id}/grid`, and only
/// after the hub's 2xx write the local copy and emit
/// [`COLLAB_GRID_CHANGED_EVENT`]. The event comes from here because this
/// device's own cache write makes its feed refresh a no-op (review I5); it
/// is emitted on every successful save, and a re-plan is idempotent.
/// Returns the grid it sent: the hub answers 204 with no body.
///
/// A hub 400 reaches the caller as the hub client's wrapped message
/// (`hub returned 400 Bad Request (put project grid): …`, review M7). Local
/// validation mirrors the hub's own text, so the wrapper shows only when the
/// two disagree.
pub async fn set_collab_project_grid(
    ctx: &ServiceContext,
    project_id: &str,
    grid: CanonicalGrid,
    emitter: Option<&dyn ProgressEmitter>,
) -> Result<CanonicalGrid, ApiError> {
    let grid = grid.normalized();
    grid.validate()
        .map_err(|m| ApiError::Invalid(m.to_string()))?;
    let Some((hub_url, token)) = crate::api::account::hub_credentials(ctx)? else {
        return Err(ApiError::SignedOut(
            "Sign in to edit the project's grid.".into(),
        ));
    };
    {
        let database = db(ctx)?;
        let conn = database.conn();
        let project = crate::api::collab_exchange::live_project(&conn, project_id)?;
        if !has_cap(
            project.is_coordinator,
            &project.gov_caps_json,
            "project.edit",
        ) {
            tracing::warn!(
                project_id,
                "grid refused: the cached card lacks project.edit"
            );
            return Err(ApiError::Forbidden(NO_GRID_CAP_MSG.into()));
        }
    }
    let client = CollabClient::new(&hub_url).map_err(client_err)?;
    client
        .put_grid(&token, project_id, &grid)
        .await
        .map_err(|e| match e {
            AccountClientError::Http {
                status: 400,
                message,
            } => ApiError::Invalid(message),
            other => client_err(other),
        })?;
    {
        let database = db(ctx)?;
        let conn = database.conn();
        crate::db::collab::set_canonical_grid(&conn, project_id, Some(&grid.canonical_json()))
            .map_err(internal)?;
    }
    tracing::info!(project_id, grid_hash = %grid.hash(), "grid saved");
    if let Some(em) = emitter {
        emit_event(
            em,
            COLLAB_GRID_CHANGED_EVENT,
            &CollabGridChanged {
                project_id: project_id.to_string(),
            },
        );
    }
    Ok(grid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::collab_exchange::test_support::{test_ctx, wire_hub};
    use crate::collab::fake_hub::FakeHub;
    use crate::collab::grid::GRID_INVALID_MSG;
    use base64::Engine;
    use std::sync::Arc;

    struct Rec(std::sync::Mutex<Vec<(String, serde_json::Value)>>);
    impl ProgressEmitter for Rec {
        fn emit_json(&self, name: &str, payload: serde_json::Value) {
            self.0.lock().unwrap().push((name.to_string(), payload));
        }
    }

    fn rec() -> Rec {
        Rec(std::sync::Mutex::new(Vec::new()))
    }

    async fn fixture(coordinator: bool) -> (tempfile::TempDir, Arc<ServiceContext>, FakeHub) {
        let hub = FakeHub::start().await;
        let (tmp, ctx) = test_ctx();
        wire_hub(&ctx, &hub.uri(), "tok");
        let b64 = base64::engine::general_purpose::STANDARD;
        hub.add_account("tok", "acc-me", "Me", &b64.encode([7u8; 32]), None);
        hub.add_account("tok-o", "acc-o", "Other", &b64.encode([8u8; 32]), None);
        hub.add_project(
            "p1",
            "m31",
            &[
                ("acc-me", "send_receive", coordinator),
                ("acc-o", "send_receive", false),
            ],
            false,
        );
        hub.set_target("p1", "M31", 10.5, 41.25, 1.0);
        let s = 2.0 / 3600.0;
        let wcs = serde_json::json!({"crval1": 10.5, "crval2": 41.25, "crpix1": 50.5, "crpix2": 40.5, "cd": [s, 0.0, 0.0, s]});
        hub.seed_frames_with("p1", "acc-o", &["w1", "w2", "w3"], "published", |f| {
            f.meta = serde_json::json!({"pixelScaleArcsec": 2.0, "naxis1": 100, "naxis2": 80, "wcs": wcs.clone()});
        });
        hub.seed_frames_with("p1", "acc-o", &["n1"], "published", |f| {
            f.meta = serde_json::json!({"pixelScaleArcsec": 2.02});
        });
        crate::api::collab::refresh_projects(&ctx).await.unwrap();
        crate::api::collab_exchange::sync_manifest(&ctx, "p1", None, None)
            .await
            .unwrap();
        (tmp, Arc::new(ctx), hub)
    }

    fn valid_grid() -> CanonicalGrid {
        CanonicalGrid {
            scale_arcsec: 2.0,
            width_px: 128,
            height_px: 128,
            crval1: 10.5,
            crval2: 41.25,
            rotation_deg: 0.0,
        }
    }

    /// Spec §6.2: the proposal reads the whole manifest — median scale,
    /// centred on the target, sized to the WCS footprints.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_proposal_reads_the_whole_manifest() {
        let (_t, ctx, _hub) = fixture(true).await;
        let p = get_collab_grid_proposal(&ctx, "p1").unwrap();
        assert_eq!(p.current, None);
        assert!(p.can_edit);
        let g = p.proposal.expect("frames carry a scale");
        assert_eq!((g.scale_arcsec, g.width_px, g.height_px), (2.0, 128, 128));
        assert_eq!((g.crval1, g.crval2, g.rotation_deg), (10.5, 41.25, 0.0));
        assert_eq!(p.frames_inside, 3, "the frame without a WCS is not judged");
    }

    /// Spec §6.3, C16: normalized before the PUT, cached only after the hub
    /// answers, returned as sent; the saving device gets exactly one
    /// `collab-grid-changed` from the command (review I5); the card carries
    /// the grid; a refresh echoing it back moves nothing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_grid_is_saved_on_the_hub_then_cached() {
        let (_t, ctx, hub) = fixture(true).await;
        let sent = CanonicalGrid {
            crval1: 370.5,
            rotation_deg: 540.0,
            ..valid_grid()
        };
        let events = rec();
        let saved = set_collab_project_grid(&ctx, "p1", sent, Some(&events))
            .await
            .unwrap();
        assert_eq!((saved.crval1, saved.rotation_deg), (10.5, 180.0));
        assert_eq!(
            *events.0.lock().unwrap(),
            vec![(
                COLLAB_GRID_CHANGED_EVENT.to_string(),
                serde_json::json!({ "projectId": "p1" })
            )],
            "exactly one event, from the command"
        );
        let on_hub =
            CanonicalGrid::from_value(&hub.project_grid("p1").expect("stored on the hub")).unwrap();
        assert_eq!(on_hub.hash(), saved.hash());
        let stored = {
            let conn = db(&ctx).unwrap().conn();
            crate::db::collab::canonical_grid(&conn, "p1").unwrap()
        };
        assert_eq!(stored.as_deref(), Some(saved.canonical_json().as_str()));
        let card = crate::api::collab::list_projects(&ctx)
            .unwrap()
            .into_iter()
            .find(|c| c.project_id == "p1")
            .unwrap();
        assert_eq!(card.canonical_grid, Some(saved));
        let report = crate::api::collab::refresh_projects_reporting(&ctx, None)
            .await
            .unwrap();
        assert!(
            report.grid_moved.is_empty(),
            "an echo moves nothing: {:?}",
            report.grid_moved
        );
        assert_eq!(
            get_collab_grid_proposal(&ctx, "p1").unwrap().current,
            Some(saved)
        );
    }

    /// The local refusals: an invalid grid before anything else, a member
    /// without `project.edit` before any request reaches the hub.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn invalid_grids_and_members_without_the_cap_are_refused_locally() {
        let (_t, ctx, hub) = fixture(false).await;
        let events = rec();
        let err = set_collab_project_grid(
            &ctx,
            "p1",
            CanonicalGrid {
                width_px: 63,
                ..valid_grid()
            },
            Some(&events),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, ApiError::Invalid(ref m) if m == GRID_INVALID_MSG),
            "{err:?}"
        );
        let err = set_collab_project_grid(&ctx, "p1", valid_grid(), Some(&events))
            .await
            .unwrap_err();
        assert!(
            matches!(err, ApiError::Forbidden(ref m) if m == NO_GRID_CAP_MSG),
            "{err:?}"
        );
        assert!(hub.project_grid("p1").is_none(), "nothing reached the hub");
        assert!(
            events.0.lock().unwrap().is_empty(),
            "a refused save emits nothing"
        );
        assert!(!get_collab_grid_proposal(&ctx, "p1").unwrap().can_edit);
    }

    /// A hub failure on the PUT leaves everything as it was: an `Err`, no
    /// cached grid, no event. (A hub 400 is not covered: the fake hub
    /// validates with the same `CanonicalGrid::validate`, so it cannot
    /// disagree with the local check without a new fake-hub feature.)
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_hub_failure_caches_nothing_and_emits_nothing() {
        let (_t, ctx, hub) = fixture(true).await;
        hub.set_failing("/projects/p1/grid", true);
        let events = rec();
        let err = set_collab_project_grid(&ctx, "p1", valid_grid(), Some(&events))
            .await
            .unwrap_err();
        tracing::debug!(?err, "expected hub failure");
        let stored = {
            let conn = db(&ctx).unwrap().conn();
            crate::db::collab::canonical_grid(&conn, "p1").unwrap()
        };
        assert_eq!(stored, None, "a failed save caches nothing");
        assert!(hub.project_grid("p1").is_none());
        assert!(
            events.0.lock().unwrap().is_empty(),
            "a failed save emits nothing"
        );
    }

    fn page_with(grid: Option<serde_json::Value>) -> crate::collab::hub_client::ProjectPageWire {
        let mut project = serde_json::json!({
            "id": "p1", "slug": "m31", "title": "M31", "status": "active",
            "requireApproval": false,
            "target": {"name": "M31", "raDeg": 10.5, "decDeg": 41.25, "radiusDeg": 1.0},
        });
        if let Some(g) = grid {
            project["canonicalGrid"] = g;
        }
        serde_json::from_value(serde_json::json!({ "project": project, "members": [] })).unwrap()
    }

    /// `grid_echo_of`: absent and null both clear; a valid grid is parsed
    /// and normalized; anything unreadable is `Malformed`.
    #[test]
    fn the_grid_echo_distinguishes_absent_grid_and_malformed() {
        use crate::api::collab::{grid_echo_of, GridEcho};
        assert!(matches!(grid_echo_of(&page_with(None)), GridEcho::Absent));
        assert!(matches!(
            grid_echo_of(&page_with(Some(serde_json::Value::Null))),
            GridEcho::Absent
        ));
        let raw = serde_json::to_value(CanonicalGrid {
            crval1: 370.5,
            rotation_deg: 540.0,
            ..valid_grid()
        })
        .unwrap();
        match grid_echo_of(&page_with(Some(raw))) {
            GridEcho::Grid(json) => {
                let g: CanonicalGrid = serde_json::from_str(&json).unwrap();
                assert_eq!((g.crval1, g.rotation_deg), (10.5, 180.0));
            }
            _ => panic!("a valid grid must parse"),
        }
        assert!(matches!(
            grid_echo_of(&page_with(Some(serde_json::json!({"bad": 1})))),
            GridEcho::Malformed
        ));
    }

    /// A refresh whose page carries an unreadable grid keeps the cached
    /// grid and reports no move; a later `null` clears it and reports one.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_malformed_echo_keeps_the_cached_grid_and_null_clears_it() {
        let (_t, ctx, hub) = fixture(true).await;
        let saved = set_collab_project_grid(&ctx, "p1", valid_grid(), None)
            .await
            .unwrap();
        hub.before_next("/projects/p1", |st| {
            st.projects.get_mut("p1").unwrap().canonical_grid = Some(serde_json::json!({"bad": 1}));
        });
        let report = crate::api::collab::refresh_projects_reporting(&ctx, None)
            .await
            .unwrap();
        assert!(report.grid_moved.is_empty(), "{:?}", report.grid_moved);
        assert_eq!(
            stored_grid(&db(&ctx).unwrap().conn(), "p1").unwrap(),
            Some(saved)
        );

        hub.before_next("/projects/p1", |st| {
            st.projects.get_mut("p1").unwrap().canonical_grid = None;
        });
        let report = crate::api::collab::refresh_projects_reporting(&ctx, None)
            .await
            .unwrap();
        assert_eq!(report.grid_moved, vec!["p1".to_string()]);
        assert_eq!(stored_grid(&db(&ctx).unwrap().conn(), "p1").unwrap(), None);
    }
}

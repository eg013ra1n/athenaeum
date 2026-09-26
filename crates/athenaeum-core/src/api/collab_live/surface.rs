//! The live exchange's command surface (Task 16, spec §14 app part; L6, L9,
//! L10, L11): what the Tauri commands and their Axum mirrors call — Sync
//! now, the live and storage status, the attention lists (changed files,
//! the deletion choice, not kept, other files), the user's answers to them,
//! the device replace / take-over and the two stream limits. Every function
//! is the whole command; the hosts only unwrap arguments.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use crate::api::collab_exchange::{bound_node, live_project, FrameLiveInfo, ProjectFrameView};
use crate::api::collab_live::holdings::member_devices;
use crate::api::collab_live::replace;
use crate::api::collab_live::storage_task::{self, ChangedAction, DeletionAction, HolderView};
use crate::api::{db, ApiError, PathPolicy};
use crate::collab::live::holders::{
    redundancy, waiting_for_publisher, FrameRef, ProjectHolders, Redundancy,
};
use crate::collab::live::presence::PresenceBook;
use crate::collab::snapshot::SnapshotMember;
use crate::collab::storage::deletions;
use crate::collab::storage::marker::{
    check_store, read_marker, CheckOutcome, StoreMarker, StoreState, UnavailableReason,
};
use crate::db::collab::CollabProjectRow;
use crate::db::collab_frames::{self as frames_db, LocalFrameRow, LocalState};
use crate::db::collab_live::{self as live_db, RefusedDesignation, RefusedDeviceKind};
use crate::services::ServiceContext;

pub use super::{CollabLiveStatus, LocalStateView, StorageStateView};
pub use replace::MARKER_MISMATCH;

// ── DTOs ────────────────────────────────────────────────────────────────────

/// A replica changed on disk and quarantined (L5) — the "Changed files" list.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ChangedFileView {
    pub frame_uuid: String,
    pub file_name: String,
    /// The changed file, left where it is.
    pub path: String,
    pub detected_at: String,
    /// The frame has a newer version than the one the file was changed from.
    pub new_version_waiting: bool,
}

/// A frame waiting for the deletion choice (L4).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ChoiceFrameView {
    pub frame_uuid: String,
    pub file_name: String,
    pub holders_online: usize,
    pub holders_total: usize,
    /// The last-copy warning: fewer than 2 other holders, offline included.
    pub at_risk: bool,
}

/// A frame this device stopped keeping (L6) — "Keep again" undoes it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct NotKeptView {
    pub frame_uuid: String,
    pub file_name: String,
    pub content_version: i32,
}

/// A file under the Collaboration root that is no frame of the project.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ForeignFileView {
    pub path: String,
    pub seen_at: String,
}

/// A project's attention lists (L4–L6, P26).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CollabAttention {
    pub changed: Vec<ChangedFileView>,
    pub awaiting_choice: Vec<ChoiceFrameView>,
    pub not_kept: Vec<NotKeptView>,
    pub other_files: Vec<ForeignFileView>,
}

/// The answer to the deletion choice (L4): `"refetch"` | `"stopKeeping"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum DeletionActionArg {
    Refetch,
    StopKeeping,
}

impl From<DeletionActionArg> for DeletionAction {
    fn from(a: DeletionActionArg) -> Self {
        match a {
            DeletionActionArg::Refetch => DeletionAction::Refetch,
            DeletionActionArg::StopKeeping => DeletionAction::StopKeeping,
        }
    }
}

/// The answer to a changed file (L5): `"refetchOriginal"` | `"delete"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum ChangedActionArg {
    RefetchOriginal,
    Delete,
}

impl From<ChangedActionArg> for ChangedAction {
    fn from(a: ChangedActionArg) -> Self {
        match a {
            ChangedActionArg::RefetchOriginal => ChangedAction::RefetchOriginal,
            ChangedActionArg::Delete => ChangedAction::Delete,
        }
    }
}

/// One row of the last-copy warning a "Stop keeping" shows first (L4, I7).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct LastCopyView {
    pub frame_uuid: String,
    pub file_name: String,
    pub holders_online: usize,
    pub holders_total: usize,
    pub at_risk: bool,
}

/// What a changed-file answer did.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ChangedFileOutcome {
    /// `true`: the changed file went to the system trash; `false`: it was
    /// deleted after the user confirmed.
    pub trashed: bool,
}

/// A Collaboration folder whose marker names ANOTHER active device of this
/// account (§9.5): replacing that device is offered.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct DeviceReplaceOfferView {
    /// The hub's device id — what `collab_replace_device` takes.
    pub device_id: String,
    pub device_name: String,
    pub last_seen_at: Option<String>,
    #[ts(type = "number | null")]
    pub offline_days: Option<i64>,
    /// Offline for more than 7 days: prompt for the replace.
    pub prompt: bool,
    /// Offline for more than 30 days: propose retiring it (never automatic).
    pub propose_retire: bool,
    /// The folder the replace applies to.
    pub path: String,
    /// The folder's marker names another store than the one this catalog
    /// recorded for it (a swapped disk): a replace would be refused.
    pub marker_mismatch: bool,
}

/// A Collaboration folder whose marker names a device this account does not
/// list (another account's device, a revoked device, a swapped disk — or the
/// hub could not be asked). Only a user-confirmed take-over adopts it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct UnknownDeviceView {
    /// The device the marker names (its public key), as the marker spells it.
    pub device_id: String,
    /// The folder — what `take_over_collab_folder` takes.
    pub path: String,
    /// Classified without the hub's answer (offline or signed out): the
    /// device may well be one of this account's; ask again when online.
    pub recorded_offline: bool,
    /// The folder's marker names another store than the one this catalog
    /// recorded for it (a swapped disk): a take-over would be refused.
    pub marker_mismatch: bool,
}

/// The Collaboration storage as the Folders / Receive pages show it (§9.1,
/// §9.5).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CollabStorageStatus {
    pub state: StorageStateView,
    /// `path_missing` | `not_a_directory` | `marker_missing` |
    /// `marker_mismatch` | `other_device` | `unknown_device`.
    pub reason: Option<String>,
    /// The designated Collaboration folder.
    pub root: Option<String>,
    pub watcher_degraded: bool,
    pub network_volume: bool,
    /// `reason = other_device` (or a refused designation of such a folder).
    pub replace: Option<DeviceReplaceOfferView>,
    /// `reason = unknown_device` (or a refused designation of such a folder).
    pub unknown_device: Option<UnknownDeviceView>,
}

/// What a replace or a take-over re-adopted.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ReplaceOutcomeView {
    pub scanned: usize,
    pub adopted: usize,
}

impl From<replace::ReplaceOutcome> for ReplaceOutcomeView {
    fn from(o: replace::ReplaceOutcome) -> Self {
        Self {
            scanned: o.scanned,
            adopted: o.adopted,
        }
    }
}

// ── holder counts ───────────────────────────────────────────────────────────

/// One project's persisted holder map, read once, with the live presence:
/// the holder counts of any of its frames without re-reading the map.
struct ProjectHolderCounts {
    project_id: String,
    map: ProjectHolders,
    presence: PresenceBook,
    members: HashSet<String>,
    /// publisher account → its devices (the "waiting for the publisher" leg).
    publishers: HashMap<String, HashSet<String>>,
    me: String,
}

impl ProjectHolderCounts {
    /// `None` when no live exchange runs (no presence, no device id).
    fn load(
        conn: &rusqlite::Connection,
        project: &CollabProjectRow,
        live: Option<&(PresenceBook, String)>,
    ) -> Result<Option<Self>, ApiError> {
        let Some((presence, me)) = live else {
            return Ok(None);
        };
        let (devices, claims) = live_db::load_holders(conn, &project.project_id)?;
        let publishers = match serde_json::from_str::<Vec<SnapshotMember>>(&project.members_json) {
            Ok(ms) => ms
                .into_iter()
                .map(|m| (m.account_id, m.nodes.into_iter().collect()))
                .collect(),
            // `member_devices` logs the same parse failure.
            Err(_) => HashMap::new(),
        };
        Ok(Some(Self {
            project_id: project.project_id.clone(),
            map: ProjectHolders::from_rows(&devices, &claims),
            presence: presence.clone(),
            members: member_devices(project),
            publishers,
            me: me.clone(),
        }))
    }

    fn frame(&self, row: &LocalFrameRow) -> FrameLiveInfo {
        let Some(frame_seq) = row.frame_seq else {
            return FrameLiveInfo::default();
        };
        let none = HashSet::new();
        let f = FrameRef {
            project_id: &self.project_id,
            frame_seq,
            content_version: row.content_version,
            publisher_devices: self
                .publishers
                .get(&row.publisher_account_id)
                .unwrap_or(&none),
        };
        let r = redundancy(&self.map, &self.presence, &self.members, &self.me, &f);
        FrameLiveInfo {
            holders_online: r.online,
            holders_total: r.total,
            waiting_for_publisher: waiting_for_publisher(
                &self.map,
                &self.presence,
                &self.members,
                &self.me,
                &f,
            ),
        }
    }
}

/// A [`HolderView`] over one project's rows: nobody is known to hold
/// anything while no live exchange runs.
struct RowHolders {
    counts: Option<ProjectHolderCounts>,
    rows: HashMap<String, LocalFrameRow>,
}

impl HolderView for RowHolders {
    fn other_holders(&self, _project_id: &str, frame_uuid: &str) -> Redundancy {
        match (&self.counts, self.rows.get(frame_uuid)) {
            (Some(c), Some(row)) => {
                let i = c.frame(row);
                Redundancy {
                    online: i.holders_online,
                    total: i.holders_total,
                }
            }
            _ => Redundancy::default(),
        }
    }
}

fn require_live(ctx: &ServiceContext, project_id: &str) -> Result<CollabProjectRow, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    live_project(&conn, project_id).map_err(|e| {
        tracing::warn!(project_id, error = %e, "collaboration project refused");
        e
    })
}

// ── the frames list and the attention lists ────────────────────────────────

/// Every cached frame of a project with its local state and live holder
/// counts (`list_collab_frames`).
pub fn list_collab_frames(
    ctx: &ServiceContext,
    project_id: &str,
) -> Result<Vec<ProjectFrameView>, ApiError> {
    let counts = {
        let db = db(ctx)?;
        let conn = db.conn();
        match crate::db::collab::get_project(&conn, project_id)? {
            Some(project) => {
                ProjectHolderCounts::load(&conn, &project, super::live_presence(ctx).as_ref())?
            }
            None => None,
        }
    };
    crate::api::collab_exchange::list_project_frames_with(ctx, project_id, |row| {
        counts.as_ref().map(|c| c.frame(row)).unwrap_or_default()
    })
}

/// A project's attention lists (L4–L6, P26).
pub fn list_collab_attention(
    ctx: &ServiceContext,
    project_id: &str,
) -> Result<CollabAttention, ApiError> {
    let project = require_live(ctx, project_id)?;
    let live = super::live_presence(ctx);
    let db = db(ctx)?;
    let conn = db.conn();
    let rows = frames_db::list_by_state(
        &conn,
        project_id,
        &[
            LocalState::Quarantined,
            LocalState::AwaitingChoice,
            LocalState::NotKept,
        ],
    )?;
    let quarantine = live_db::list_quarantine(&conn, project_id)?;
    let counts = ProjectHolderCounts::load(&conn, &project, live.as_ref())?;
    let foreign = frames_db::list_foreign_files(&conn, project_id)?;
    drop(conn);

    let by_uuid: HashMap<&str, &LocalFrameRow> =
        rows.iter().map(|r| (r.frame_uuid.as_str(), r)).collect();
    let changed = quarantine
        .into_iter()
        .filter_map(|q| {
            let Some(row) = by_uuid
                .get(q.frame_uuid.as_str())
                .filter(|r| r.local_state == LocalState::Quarantined)
            else {
                tracing::debug!(project_id, frame_uuid = %q.frame_uuid, "quarantine row without a quarantined frame; not listed");
                return None;
            };
            Some(ChangedFileView {
                frame_uuid: q.frame_uuid,
                file_name: row.file_name.clone(),
                path: q.path,
                detected_at: q.detected_at,
                new_version_waiting: row.content_version > q.quarantined_version,
            })
        })
        .collect();
    let awaiting_choice = rows
        .iter()
        .filter(|r| r.local_state == LocalState::AwaitingChoice)
        .map(|r| {
            let i = counts.as_ref().map(|c| c.frame(r)).unwrap_or_default();
            ChoiceFrameView {
                frame_uuid: r.frame_uuid.clone(),
                file_name: r.file_name.clone(),
                holders_online: i.holders_online,
                holders_total: i.holders_total,
                at_risk: deletions::last_copy_warning(i.holders_total),
            }
        })
        .collect();
    let not_kept = rows
        .iter()
        .filter(|r| r.local_state == LocalState::NotKept)
        .map(|r| NotKeptView {
            frame_uuid: r.frame_uuid.clone(),
            file_name: r.file_name.clone(),
            content_version: r.content_version,
        })
        .collect();
    let other_files = foreign
        .into_iter()
        .map(|(path, seen_at)| ForeignFileView { path, seen_at })
        .collect();
    Ok(CollabAttention {
        changed,
        awaiting_choice,
        not_kept,
        other_files,
    })
}

/// Answer the deletion choice (L4) for every awaiting frame of the project,
/// or only the named ones; the live exchange re-reads the project's need
/// set. Returns the frames moved.
pub fn resolve_collab_deletions(
    ctx: &ServiceContext,
    project_id: &str,
    frame_uuids: Option<Vec<String>>,
    action: DeletionActionArg,
) -> Result<usize, ApiError> {
    require_live(ctx, project_id)?;
    let n =
        storage_task::resolve_deletions(ctx, project_id, frame_uuids.as_deref(), action.into())?;
    super::notify_local_change(ctx, project_id);
    Ok(n)
}

/// The last-copy warning (L4, I7) for the frames a "Stop keeping" would
/// drop. Without a live exchange nobody is known to hold them: every row is
/// at risk.
pub fn preview_collab_stop_keeping(
    ctx: &ServiceContext,
    project_id: &str,
    frame_uuids: Vec<String>,
) -> Result<Vec<LastCopyView>, ApiError> {
    let project = require_live(ctx, project_id)?;
    let holders = {
        let db = db(ctx)?;
        let conn = db.conn();
        let counts =
            ProjectHolderCounts::load(&conn, &project, super::live_presence(ctx).as_ref())?;
        let mut rows = HashMap::new();
        for uuid in &frame_uuids {
            if let Some(row) = frames_db::get(&conn, project_id, uuid)? {
                rows.insert(uuid.clone(), row);
            }
        }
        RowHolders { counts, rows }
    };
    let report = storage_task::last_copy_report(ctx, &holders, project_id, &frame_uuids)?;
    Ok(report
        .into_iter()
        .map(|r| LastCopyView {
            frame_uuid: r.frame_uuid,
            file_name: r.file_name,
            holders_online: r.holders_online,
            holders_total: r.holders_total,
            at_risk: r.at_risk,
        })
        .collect())
}

/// "Keep again" (L6), per frame or for all; the live exchange re-reads the
/// project's need set. Returns the frames moved.
pub fn keep_collab_frames_again(
    ctx: &ServiceContext,
    project_id: &str,
    frame_uuids: Option<Vec<String>>,
) -> Result<usize, ApiError> {
    require_live(ctx, project_id)?;
    let n = storage_task::keep_again(ctx, project_id, frame_uuids.as_deref())?;
    super::notify_local_change(ctx, project_id);
    Ok(n)
}

/// Answer a changed file (L5): re-fetch the original (the edited file goes
/// to the system trash, or is deleted once `confirmed_delete`) or delete it
/// (`confirmed_delete` required; the frame is then not kept).
pub async fn resolve_collab_changed_file(
    ctx: &ServiceContext,
    project_id: &str,
    frame_uuid: &str,
    action: ChangedActionArg,
    confirmed_delete: bool,
) -> Result<ChangedFileOutcome, ApiError> {
    require_live(ctx, project_id)?;
    let Some(node) = bound_node(ctx).await else {
        tracing::warn!(
            project_id,
            frame_uuid,
            "changed file not resolved: no iroh node bound"
        );
        return Err(ApiError::Invalid(
            "collaboration is not running".to_string(),
        ));
    };
    let out = storage_task::resolve_changed_file(
        ctx,
        &node,
        project_id,
        frame_uuid,
        action.into(),
        confirmed_delete,
    )
    .await?;
    super::notify_local_change(ctx, project_id);
    Ok(ChangedFileOutcome {
        trashed: out.trashed,
    })
}

// ── live status, Sync now, stream limits ───────────────────────────────────

/// The live status (P27); `off` when no live exchange runs.
pub fn get_collab_live_status(ctx: &ServiceContext) -> CollabLiveStatus {
    super::status(ctx)
}

/// Sync now (L10, P26): back-offs cleared, the stream reopened, then a
/// digest check per project and a stat sweep.
pub fn collab_sync_now(ctx: &ServiceContext) -> Result<(), ApiError> {
    super::sync_now(ctx)
}

/// `collab.max_upload_streams` (L11): refused outside
/// [`COLLAB_UPLOAD_STREAMS_RANGE`](crate::settings::COLLAB_UPLOAD_STREAMS_RANGE),
/// else stored and applied to the bound node at once.
pub async fn set_collab_max_upload_streams(ctx: &ServiceContext, n: usize) -> Result<(), ApiError> {
    let range = crate::settings::COLLAB_UPLOAD_STREAMS_RANGE;
    check_range("collab.max_upload_streams", n, &range)?;
    persist_streams(ctx, crate::settings::keys::COLLAB_MAX_UPLOAD_STREAMS, n)?;
    if let Some(node) = bound_node(ctx).await {
        node.set_collab_upload_limit(n);
    }
    tracing::info!(streams = n, "collab upload stream limit set");
    Ok(())
}

/// `collab.max_receive_streams` (L11): refused outside
/// [`COLLAB_RECEIVE_STREAMS_RANGE`](crate::settings::COLLAB_RECEIVE_STREAMS_RANGE),
/// else stored and applied to the live exchange at once.
pub fn set_collab_max_receive_streams(ctx: &ServiceContext, n: usize) -> Result<(), ApiError> {
    let range = crate::settings::COLLAB_RECEIVE_STREAMS_RANGE;
    check_range("collab.max_receive_streams", n, &range)?;
    persist_streams(ctx, crate::settings::keys::COLLAB_MAX_RECEIVE_STREAMS, n)?;
    super::set_receive_streams(ctx, n);
    tracing::info!(streams = n, "collab receive stream limit set");
    Ok(())
}

fn check_range(
    key: &str,
    n: usize,
    range: &std::ops::RangeInclusive<usize>,
) -> Result<(), ApiError> {
    if range.contains(&n) {
        return Ok(());
    }
    tracing::warn!(streams = n, "collab stream limit refused: out of range");
    Err(ApiError::Invalid(format!(
        "{key} must be {}..={}",
        range.start(),
        range.end()
    )))
}

fn persist_streams(ctx: &ServiceContext, key: &str, n: usize) -> Result<(), ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    ctx.settings
        .persist_setting(&conn, key, &n.to_string())
        .map_err(|e| {
            tracing::error!(streams = n, error = %e, "collab stream limit could not be stored");
            ApiError::Internal(format!("store {key}: {e}"))
        })
}

// ── storage status, replace, take-over ─────────────────────────────────────

/// A folder argument from the UI (replace / take-over): absolute, and never
/// stepping up with `..` — [`PathPolicy::check`] is lexical, so a `..` could
/// otherwise walk out of an allowed root before anything canonicalizes it.
pub fn folder_arg(raw: &str) -> Result<PathBuf, ApiError> {
    let t = raw.trim();
    let p = PathBuf::from(t);
    if t.is_empty() || !p.is_absolute() {
        tracing::warn!(path = t, "folder argument refused: not an absolute path");
        return Err(ApiError::Invalid(
            "the folder must be an absolute path".to_string(),
        ));
    }
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        tracing::warn!(path = t, "folder argument refused: it contains '..'");
        return Err(ApiError::Invalid(
            "the folder path must not contain '..'".to_string(),
        ));
    }
    Ok(p)
}

/// The spelling the refusal record and the take-over compare: canonical and
/// normalized when the folder exists, else as given.
fn canonical(p: &Path) -> PathBuf {
    p.canonicalize()
        .map(|c| crate::api::scan_roots::normalize_path(&c))
        .unwrap_or_else(|_| p.to_path_buf())
}

async fn marker_at(path: &Path) -> Result<Option<StoreMarker>, ApiError> {
    let p = path.to_path_buf();
    let read = tokio::task::spawn_blocking(move || read_marker(&p))
        .await
        .map_err(|e| ApiError::Internal(format!("marker read task join: {e}")))?;
    read.map_err(|e| {
        tracing::warn!(path = %path.display(), error = %e, "collaboration marker unreadable");
        ApiError::Internal(format!("read storage marker: {e}"))
    })
}

fn reason_str(r: &UnavailableReason) -> &'static str {
    match r {
        UnavailableReason::PathMissing => "path_missing",
        UnavailableReason::NotADirectory => "not_a_directory",
        UnavailableReason::MarkerMissing => "marker_missing",
        UnavailableReason::MarkerMismatch => "marker_mismatch",
        UnavailableReason::OtherDevice { .. } => "other_device",
    }
}

/// Which kind of device `device` (a marker's device) is, asking the hub the
/// way a designation does (spec §9.5, T7): one of this account's → the
/// replace offer; not listed → unknown; the hub cannot be asked → unknown,
/// labelled offline. The answer is recorded as the folder's refusal (so a
/// take-over of an unknown device's folder is possible, and only then).
/// `sets_reason`: the refusal explains the status (`reason`).
async fn classify_marker_device(
    ctx: &ServiceContext,
    path: &Path,
    device: &str,
    sets_reason: bool,
    out: &mut CollabStorageStatus,
) -> Result<(), ApiError> {
    let canon = canonical(path);
    let path_str = canon.to_string_lossy().to_string();
    let marker_mismatch = match replace::check_no_recorded_marker_mismatch(ctx, path, &canon) {
        Ok(()) => false,
        Err(ApiError::Conflict(_)) => true,
        Err(e) => {
            tracing::warn!(path = %path_str, error = %e, "recorded storage marker could not be compared; no mismatch assumed");
            false
        }
    };
    let (kind, offline) = match replace::replace_offer(ctx, device).await {
        Ok(Some(o)) => {
            out.replace = Some(DeviceReplaceOfferView {
                device_id: o.device_id,
                device_name: o.device_name,
                last_seen_at: o.last_seen_at,
                offline_days: o.offline_days,
                prompt: o.prompt,
                propose_retire: o.propose_retire,
                path: path_str.clone(),
                marker_mismatch,
            });
            (RefusedDeviceKind::Other, false)
        }
        Ok(None) => (RefusedDeviceKind::Unknown, false),
        Err(e) => {
            tracing::warn!(path = %path_str, device, error = %e, "could not confirm whether the marker's device is still active");
            (RefusedDeviceKind::Unknown, true)
        }
    };
    if kind == RefusedDeviceKind::Unknown {
        out.unknown_device = Some(UnknownDeviceView {
            device_id: device.to_string(),
            path: path_str.clone(),
            recorded_offline: offline,
            marker_mismatch,
        });
    }
    if sets_reason {
        out.reason = Some(
            match kind {
                RefusedDeviceKind::Other => "other_device",
                RefusedDeviceKind::Unknown => "unknown_device",
            }
            .to_string(),
        );
    }
    let record = RefusedDesignation {
        path: path_str,
        device_id: device.to_string(),
        kind,
        offline,
    };
    let db = db(ctx)?;
    let conn = db.conn();
    if live_db::refused_designation_detail(&conn)?.as_ref() != Some(&record) {
        if let Err(e) = live_db::record_refused_designation(
            &conn,
            &record.path,
            &record.device_id,
            record.kind,
            record.offline,
        ) {
            tracing::warn!(path = %record.path, error = %e, "recording the storage refusal failed");
        } else {
            tracing::info!(
                path = %record.path,
                device = %record.device_id,
                kind = ?record.kind,
                refused = true,
                "collaboration folder refusal classified"
            );
        }
    }
    Ok(())
}

/// The Collaboration storage (§9.1, §9.5): the designated folder's marker
/// check, the watcher state, and — when the folder's marker names another
/// device (or a designation was refused for that reason) — the replace offer
/// for one of this account's devices, or the take-over of an unknown one's.
pub async fn get_collab_storage_status(
    ctx: &ServiceContext,
) -> Result<CollabStorageStatus, ApiError> {
    let live = super::status(ctx);
    let root = crate::api::scan_roots::get_collaboration_dir(ctx)?;
    let mut out = CollabStorageStatus {
        state: StorageStateView::NotSet,
        reason: None,
        root: root.clone(),
        watcher_degraded: live.watcher_degraded,
        network_volume: live.network_volume,
        replace: None,
        unknown_device: None,
    };
    let me = crate::api::account::own_device_id(ctx)?;
    let root_canon = root.as_deref().map(|r| canonical(Path::new(r)));

    if let Some(root) = &root {
        let root_path = PathBuf::from(root);
        let recorded = {
            let db = db(ctx)?;
            let conn = db.conn();
            if live_db::store_marker_path(&conn)?.as_deref() == Some(root.as_str()) {
                live_db::recorded_store_marker(&conn)?
            } else {
                None
            }
        };
        let (p, m) = (root_path.clone(), me.clone());
        let outcome = tokio::task::spawn_blocking(move || check_store(&p, recorded.as_ref(), &m))
            .await
            .map_err(|e| ApiError::Internal(format!("storage check task join: {e}")))?;
        match outcome {
            // A folder with no marker yet (or one naming this device, not
            // yet recorded): the live exchange adopts it on its next check.
            CheckOutcome::Adopt(_) | CheckOutcome::State(StoreState::Available) => {
                out.state = StorageStateView::Available
            }
            CheckOutcome::State(StoreState::ReadOnly) => out.state = StorageStateView::ReadOnly,
            CheckOutcome::State(StoreState::Unavailable(r)) => {
                out.state = StorageStateView::Unavailable;
                out.reason = Some(reason_str(&r).to_string());
                if let UnavailableReason::OtherDevice { device_id } = r {
                    classify_marker_device(ctx, &root_path, &device_id, true, &mut out).await?;
                }
            }
        }
    }

    // A refused designation of another folder (the reinstall flow: the old
    // folder was picked and refused) is surfaced too — while its marker
    // still names another device.
    if out.replace.is_none() && out.unknown_device.is_none() {
        let refusal = {
            let db = db(ctx)?;
            let conn = db.conn();
            live_db::refused_designation_detail(&conn)?
        };
        if let Some(r) = refusal.filter(|r| root_canon.as_deref() != Some(Path::new(&r.path))) {
            let path = PathBuf::from(&r.path);
            match marker_at(&path).await {
                Ok(Some(m)) if m.device_id != me => {
                    classify_marker_device(ctx, &path, &m.device_id, root.is_none(), &mut out)
                        .await?;
                }
                Ok(_) => tracing::debug!(
                    path = %r.path,
                    "the refused folder's marker no longer names another device"
                ),
                Err(e) => {
                    tracing::warn!(path = %r.path, error = %e, "the refused folder could not be checked")
                }
            }
        }
    }
    Ok(out)
}

/// The folder a replace applies to when the UI names none: the designated
/// Collaboration folder when its marker names another device, else the
/// folder whose designation was refused.
async fn replace_target(ctx: &ServiceContext) -> Result<PathBuf, ApiError> {
    let me = crate::api::account::own_device_id(ctx)?;
    if let Some(root) = crate::api::scan_roots::get_collaboration_dir(ctx)? {
        let root = PathBuf::from(root);
        if marker_at(&root).await?.is_some_and(|m| m.device_id != me) {
            return Ok(root);
        }
    }
    let refusal = {
        let db = db(ctx)?;
        let conn = db.conn();
        live_db::refused_designation_detail(&conn)?
    };
    match refusal {
        Some(r) => Ok(PathBuf::from(r.path)),
        None => {
            tracing::warn!("device replace refused: no folder names another device");
            Err(ApiError::Invalid(
                "no Collaboration folder names another device".to_string(),
            ))
        }
    }
}

fn notify_every_project(ctx: &ServiceContext) {
    let projects = match db(ctx).and_then(|d| Ok(crate::db::collab::list_projects(&d.conn())?)) {
        Ok(ps) => ps,
        Err(e) => {
            tracing::error!(error = %e, "projects could not be listed; the live exchange is not told");
            return;
        }
    };
    for p in projects {
        super::notify_local_change(ctx, &p.project_id);
    }
}

/// Replace `device_id` (a hub device id from the offer) as the owner of
/// `root` — the offer's folder when `None` (§9.5; T7's verified replace).
pub async fn collab_replace_device(
    ctx: &ServiceContext,
    device_id: &str,
    root: Option<&str>,
    policy: &PathPolicy,
) -> Result<ReplaceOutcomeView, ApiError> {
    let root = match root {
        Some(r) => folder_arg(r)?,
        None => replace_target(ctx).await?,
    };
    let out = replace::replace_device(ctx, device_id, &root, policy).await?;
    notify_every_project(ctx);
    Ok(out.into())
}

/// Take over a folder whose marker names a device this account does not
/// list (T7's take-over): only after a recorded unknown-device refusal of
/// that folder, and only with the user's confirmation (`confirmed`).
pub async fn take_over_collab_folder(
    ctx: &ServiceContext,
    root: &str,
    confirmed: bool,
    policy: &PathPolicy,
) -> Result<ReplaceOutcomeView, ApiError> {
    let root = folder_arg(root)?;
    let out = replace::take_over_collab_folder(ctx, &root, policy, confirmed).await?;
    notify_every_project(ctx);
    Ok(out.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::collab_live::test_support as ts;
    use crate::db::collab_frames::{self as frames_db, LocalState};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn attention_lists_every_kind_and_actions_move_frames_between_them() {
        let rig = ts::landed_rig(4).await;
        let pid = rig.frames[0].0.clone();
        {
            let conn = crate::api::db(&rig.ctx).unwrap().conn();
            frames_db::set_local_state(&conn, &pid, &rig.frames[1].1, LocalState::AwaitingChoice)
                .unwrap();
            frames_db::set_local_state(&conn, &pid, &rig.frames[2].1, LocalState::NotKept).unwrap();
            crate::db::collab_frames::record_foreign_file(
                &conn,
                &rig.root.join("m31/stray.fits").to_string_lossy(),
                Some(&pid),
                None,
            )
            .unwrap();
        }
        ts::overwrite_same_size(&rig.frames[0].2);
        rig.engine().local_check(&pid, &rig.frames[0].1).await; // → quarantined
        let a = list_collab_attention(&rig.ctx, &pid).unwrap();
        assert_eq!(
            (
                a.changed.len(),
                a.awaiting_choice.len(),
                a.not_kept.len(),
                a.other_files.len()
            ),
            (1, 1, 1, 1)
        );
        assert!(!a.changed[0].new_version_waiting);
        // No live runtime: nobody is known to hold anything — at risk.
        assert!(a.awaiting_choice[0].at_risk);
        let preview =
            preview_collab_stop_keeping(&rig.ctx, &pid, vec![rig.frames[1].1.clone()]).unwrap();
        assert_eq!(preview.len(), 1);
        assert!(preview[0].at_risk && preview[0].holders_total == 0);

        // The frames list carries the local state in place of the retired
        // on-disk / declined / holder-count fields.
        let frames = list_collab_frames(&rig.ctx, &pid).unwrap();
        let state_of = |uuid: &str| {
            frames
                .iter()
                .find(|f| f.frame_uuid == uuid)
                .map(|f| f.local_state)
                .unwrap()
        };
        assert_eq!(state_of(&rig.frames[0].1), LocalStateView::Quarantined);
        assert_eq!(state_of(&rig.frames[1].1), LocalStateView::AwaitingChoice);
        assert_eq!(state_of(&rig.frames[2].1), LocalStateView::NotKept);
        assert_eq!(state_of(&rig.frames[3].1), LocalStateView::Held);

        assert_eq!(
            resolve_collab_deletions(&rig.ctx, &pid, None, DeletionActionArg::StopKeeping).unwrap(),
            1
        );
        assert_eq!(keep_collab_frames_again(&rig.ctx, &pid, None).unwrap(), 2);
        let a = list_collab_attention(&rig.ctx, &pid).unwrap();
        assert!(a.awaiting_choice.is_empty() && a.not_kept.is_empty());
    }

    #[tokio::test]
    async fn storage_status_reports_an_other_device_root_with_a_replace_offer() {
        let (_t, ctx, hub) = ts::signed_in_rig().await;
        let root = ts::collab_root(&ctx);
        hub.add_device(
            "acc-me",
            "OLD-DEV",
            "old-id",
            "Old laptop",
            Some(chrono::Utc::now() - chrono::Duration::days(10)),
        );
        crate::collab::storage::marker::write_marker(
            &root,
            &crate::collab::storage::marker::StoreMarker {
                store_id: "s".into(),
                device_id: "OLD-DEV".into(),
            },
        )
        .unwrap();
        let s = get_collab_storage_status(&ctx).await.unwrap();
        assert_eq!(s.state, StorageStateView::Unavailable);
        assert_eq!(s.reason.as_deref(), Some("other_device"));
        assert!(s.unknown_device.is_none());
        let offer = s.replace.expect("an offer for an account device");
        assert!(offer.prompt && !offer.propose_retire);
        assert_eq!(offer.device_id, "old-id");
        assert_eq!(offer.device_name, "Old laptop");
        assert_eq!(offer.offline_days, Some(10));
        assert_eq!(offer.path, s.root.clone().unwrap());
        // The status read records the classification: an OtherDevice
        // refusal never unlocks a take-over.
        let refusal =
            crate::db::collab_live::refused_designation(&crate::api::db(&ctx).unwrap().conn())
                .unwrap()
                .expect("the classification is recorded");
        assert_eq!(refusal.2, crate::db::collab_live::RefusedDeviceKind::Other);
        let err = take_over_collab_folder(&ctx, &offer.path, true, &PathPolicy::AllowAll)
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::Invalid(_)), "{err:?}");
    }

    #[tokio::test]
    async fn storage_status_classifies_an_unlisted_marker_device_as_unknown() {
        let (_t, ctx, _hub) = ts::signed_in_rig().await;
        let root = ts::collab_root(&ctx);
        crate::collab::storage::marker::write_marker(
            &root,
            &crate::collab::storage::marker::StoreMarker {
                store_id: "swapped".into(),
                device_id: "GHOST".into(),
            },
        )
        .unwrap();
        let s = get_collab_storage_status(&ctx).await.unwrap();
        assert_eq!(s.state, StorageStateView::Unavailable);
        assert_eq!(s.reason.as_deref(), Some("unknown_device"));
        assert!(s.replace.is_none());
        let u = s.unknown_device.expect("an unknown-device refusal");
        assert_eq!(u.device_id, "GHOST");
        assert_eq!(u.path, s.root.clone().unwrap());
        assert!(!u.recorded_offline);
        // A different store id than the one this catalog recorded for the
        // folder: a swapped disk — the take-over would be refused, and the
        // status says so up front.
        assert!(u.marker_mismatch);
        let refusal = crate::db::collab_live::refused_designation_detail(
            &crate::api::db(&ctx).unwrap().conn(),
        )
        .unwrap()
        .expect("recorded");
        assert_eq!(
            refusal.kind,
            crate::db::collab_live::RefusedDeviceKind::Unknown
        );
        assert!(!refusal.offline);
        let err = take_over_collab_folder(&ctx, &u.path, true, &PathPolicy::AllowAll)
            .await
            .unwrap_err();
        match err {
            ApiError::Conflict(m) => assert!(m.starts_with(MARKER_MISMATCH), "{m}"),
            other => panic!("expected the typed marker mismatch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_unreachable_device_list_is_recorded_offline_as_unknown() {
        let (_t, ctx, hub) = ts::signed_in_rig().await;
        let root = ts::collab_root(&ctx);
        hub.add_device("acc-me", "OLD-DEV", "old-id", "Old laptop", None);
        crate::collab::storage::marker::write_marker(
            &root,
            &crate::collab::storage::marker::StoreMarker {
                store_id: "s".into(),
                device_id: "OLD-DEV".into(),
            },
        )
        .unwrap();
        hub.set_failing("/devices", true);
        let s = get_collab_storage_status(&ctx).await.unwrap();
        assert!(s.replace.is_none());
        let u = s
            .unknown_device
            .expect("unknown while the hub cannot be asked");
        assert!(u.recorded_offline);
        // Asked again once the hub answers: the device is an account device.
        hub.set_failing("/devices", false);
        let s = get_collab_storage_status(&ctx).await.unwrap();
        assert!(s.unknown_device.is_none());
        assert_eq!(s.replace.expect("an offer now").device_id, "old-id");
    }

    #[tokio::test]
    async fn take_over_is_refused_without_a_recorded_unknown_refusal() {
        let (_t, ctx, _hub) = ts::signed_in_rig().await;
        let root = ts::collab_root(&ctx);
        crate::collab::storage::marker::write_marker(
            &root,
            &crate::collab::storage::marker::StoreMarker {
                store_id: "s".into(),
                device_id: "GHOST".into(),
            },
        )
        .unwrap();
        // No status read, no designation attempt: nothing was recorded.
        let err =
            take_over_collab_folder(&ctx, &root.to_string_lossy(), true, &PathPolicy::AllowAll)
                .await
                .unwrap_err();
        assert!(matches!(err, ApiError::Invalid(_)), "{err:?}");
    }

    #[test]
    fn a_folder_argument_with_a_parent_step_or_relative_is_refused() {
        assert!(matches!(
            folder_arg("/data/collab/../etc"),
            Err(ApiError::Invalid(_))
        ));
        assert!(matches!(folder_arg("collab"), Err(ApiError::Invalid(_))));
        assert!(matches!(folder_arg("   "), Err(ApiError::Invalid(_))));
        let ok = if cfg!(windows) {
            r"C:\data\collab"
        } else {
            "/data/collab"
        };
        assert_eq!(
            folder_arg(&format!(" {ok} ")).unwrap(),
            std::path::PathBuf::from(ok)
        );
    }

    #[tokio::test]
    async fn replace_and_take_over_refuse_a_parent_step_before_touching_anything() {
        let (_t, ctx, _hub) = ts::signed_in_rig().await;
        let bad = format!("{}/../elsewhere", ts::collab_root(&ctx).display());
        let err = take_over_collab_folder(&ctx, &bad, true, &PathPolicy::AllowAll)
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::Invalid(_)), "{err:?}");
        let err = collab_replace_device(&ctx, "old-id", Some(&bad), &PathPolicy::AllowAll)
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::Invalid(_)), "{err:?}");
    }

    #[tokio::test]
    async fn stream_limits_persist_and_refuse_out_of_range_on_both_setters() {
        let (_t, ctx, _hub) = ts::signed_in_rig().await;
        set_collab_max_upload_streams(&ctx, 16).await.unwrap();
        set_collab_max_receive_streams(&ctx, 4).unwrap();
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            assert_eq!(
                ctx.settings.get_collab_max_upload_streams(&conn).unwrap(),
                16
            );
            assert_eq!(
                ctx.settings.get_collab_max_receive_streams(&conn).unwrap(),
                4
            );
        }
        for n in [0, 65, 500] {
            assert!(
                matches!(
                    set_collab_max_upload_streams(&ctx, n).await,
                    Err(ApiError::Invalid(_))
                ),
                "upload {n}"
            );
        }
        for n in [0, 33] {
            assert!(
                matches!(
                    set_collab_max_receive_streams(&ctx, n),
                    Err(ApiError::Invalid(_))
                ),
                "receive {n}"
            );
        }
        let conn = crate::api::db(&ctx).unwrap().conn();
        assert_eq!(
            ctx.settings.get_collab_max_upload_streams(&conn).unwrap(),
            16
        );
        assert_eq!(
            ctx.settings.get_collab_max_receive_streams(&conn).unwrap(),
            4
        );
    }

    #[test]
    fn deletion_choice_carries_a_stable_dedupe_key() {
        let p =
            crate::api::collab_live::CollabDeletionChoice::new(3, vec!["p2".into(), "p1".into()]);
        assert_eq!(p.dedupe_key, "collab-deletion-choice:p1,p2");
        assert_eq!(p.project_ids, vec!["p1".to_string(), "p2".to_string()]);
    }
}

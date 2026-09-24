//! The collab v3 per-frame exchange (wave 2): the hub version poll and the
//! manifest delta into `project_frames_local` (Task 8), the replication pass
//! — disk truth, loss guard, need set, fetch and landing (Task 9) — the
//! auto-sync worker that drives both, the replication policy and loss
//! commands, and the project-scoped WBPP export. The package layer this module
//! once held (package dirs, push-seed, the collab sender runtime, the swarm
//! package download) is retired (Task 12, plan P12/P26).
//!
//! Ungated (no render gate) except where an item says otherwise: depends only
//! on `db`, `sync`, `sharing`, `collab`, `package`, so it compiles in the
//! headless (`--no-default-features`) build.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};

use crate::account::keys::{device_key_path, DeviceKey};
use crate::api::{db, ApiError};
use crate::collab::hub_client::CollabClient;
use crate::collab::snapshot::{own_display_name, SnapshotMember};
use crate::db::collab_frames::{FrameOrigin, LocalFrameRow};
use crate::events::ProgressEmitter;
use crate::export::models::WbppExportConfig;
use crate::services::ServiceContext;
use crate::sharing::types::NodeId;
use crate::sharing::{ProviderEvent, ProviderTelemetrySink};
use crate::sync::{node_id_hex, pairing};

/// Map an [`AccountClientError`](crate::account::AccountClientError) onto the api
/// boundary (mirrors `api::collab::client_err`).
fn client_err(e: crate::account::AccountClientError) -> ApiError {
    use crate::account::AccountClientError as E;
    match e {
        E::RateLimited => {
            ApiError::Invalid("Too many requests — wait a minute and try again.".into())
        }
        E::Unauthorized => {
            ApiError::SignedOut("Signed out or device revoked — sign in again.".into())
        }
        E::SecondPrimary(m) | E::DeviceConflict(m) => ApiError::Conflict(m),
        E::PeerValidation(m) | E::BadRequest(m) => ApiError::Invalid(m),
        E::DuplicateName => ApiError::Invalid("name already in use".into()),
        E::Forbidden => {
            ApiError::Forbidden("The account's role may not perform this action.".into())
        }
        // The per-frame calls here (the version poll, the manifest sync)
        // produce this variant; the deprecated package-api ones never do.
        E::CollabApiOutdated => {
            crate::account::client::warn_collab_api_outdated_once();
            ApiError::Conflict(crate::account::client::COLLAB_API_OUTDATED_MSG.into())
        }
        E::Network(m) => ApiError::Internal(format!("Hub request failed: {m}")),
    }
}

/// The wakeup the auto-replication worker waits on between passes (spec §3.3:
/// a pass right after a version poll saw a project move). A module static
/// because the producers (the version poll, the loss commands, the per-project
/// "sync now") and the consumer (the worker spawned by
/// [`spawn_collab_auto_sync`]) have no shared owner, and a static keeps both
/// arming sites in `api::sync` and every command signature untouched.
static AUTO_SYNC_KICK: std::sync::OnceLock<tokio::sync::Notify> = std::sync::OnceLock::new();

fn auto_sync_kick() -> &'static tokio::sync::Notify {
    AUTO_SYNC_KICK.get_or_init(tokio::sync::Notify::new)
}

// ── Version poll + manifest delta (collab v3 wave 2, Task 8; R19, P9) ────────
//
// The hub keeps one `version` per project, bumped by every change a device
// must see (a manifest row, membership, caps, thresholds, dictionary). The
// auto-sync worker asks `GET /me/project-versions` every
// [`COLLAB_VERSION_POLL_INTERVAL`]; a project whose version differs from the
// cached `hub_version` gets ONE project refresh (joins, losses, caps,
// thresholds, dictionary) and a paged manifest delta from its cursor. A quiet
// poll is one request.

/// How often the auto-sync worker asks the hub whether any project moved.
pub const COLLAB_VERSION_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

/// Grace before the FIRST version poll of a session (the pass waits 90 s).
const COLLAB_VERSION_POLL_STARTUP_DELAY: std::time::Duration = std::time::Duration::from_secs(5);

/// Rows per manifest page — the hub's cap.
const MANIFEST_PAGE_LIMIT: u32 = 1000;

/// The per-kind outcome event of a manifest sync ([`CollabFramesChange`]).
pub const COLLAB_FRAMES_CHANGED_EVENT: &str = "collab-frames-changed";

/// What a manifest delta changed, from this device's point of view.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    ts_rs::TS,
)]
#[serde(rename_all = "camelCase")]
pub enum FramesChangeKind {
    /// Another member's frame became visible as published.
    NewFrames,
    /// Another member's frame awaits moderation (visible to moderators only).
    PendingFrames,
    /// My pending frame was published.
    Approved,
    /// My frame was rejected.
    Rejected,
    /// A frame was excluded from the project (`accepted` true → false).
    Excluded,
    /// Another member published a new content version of a frame.
    NewVersions,
}

impl FramesChangeKind {
    /// The wire spelling (the serde name), for logs.
    fn as_str(self) -> &'static str {
        match self {
            FramesChangeKind::NewFrames => "newFrames",
            FramesChangeKind::PendingFrames => "pendingFrames",
            FramesChangeKind::Approved => "approved",
            FramesChangeKind::Rejected => "rejected",
            FramesChangeKind::Excluded => "excluded",
            FramesChangeKind::NewVersions => "newVersions",
        }
    }
}

/// One `collab-frames-changed` event: how many rows of one kind a manifest
/// sync applied for one project. Emitted once per non-zero kind. Also
/// `Deserialize`: [`ChangeCollector`] decodes it back out of its own emitted
/// JSON to build `refresh_collab_frames`'s return value.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CollabFramesChange {
    pub project_id: String,
    pub kind: FramesChangeKind,
    pub count: usize,
}

/// Classify one manifest row against the local row it replaces (`None` =
/// never seen). Pure. A row can carry more than one kind (an exclusion and a
/// new version in one delta).
pub(crate) fn classify_frame_change(
    prev: Option<&crate::db::collab_frames::LocalFrameRow>,
    v: &crate::collab::hub_client::FrameViewWire,
) -> Vec<FramesChangeKind> {
    use FramesChangeKind as K;
    let mut out = Vec::new();
    let prev_state = prev.map(|p| p.state.as_str());
    if v.own {
        // Only a row this device already tracked: a first sync of an old
        // account must not replay every historical decision as news.
        if prev_state == Some("pending") && v.state == "published" {
            out.push(K::Approved);
        }
        if prev.is_some() && prev_state != Some("rejected") && v.state == "rejected" {
            out.push(K::Rejected);
        }
    } else {
        if v.state == "published" && prev_state != Some("published") {
            out.push(K::NewFrames);
        } else if prev.is_some_and(|p| v.content_version > p.content_version) {
            out.push(K::NewVersions);
        }
        // Pending rows of another member reach only a moderator's manifest.
        if v.state == "pending" && prev_state != Some("pending") {
            out.push(K::PendingFrames);
        }
    }
    if prev.is_some_and(|p| p.accepted) && !v.accepted {
        out.push(K::Excluded);
    }
    out
}

/// Pull one project's manifest delta into `project_frames_local` (P9).
///
/// 1. The cursor is `manifest_cursor`, or 0 with a prune when the caps the
///    last sync saw (`synced_caps_json`) differ from the current ones
///    (`gov_caps_json`, written by the project refresh) — the hub README's
///    client caps rule.
/// 2. Pages of 1000 are followed through `next` until `hasMore` is false;
///    each row is classified against the local row, then upserted (an own
///    row takes the hub's columns and keeps its local ones).
/// 3. On a caps change, every replica row the fresh fetch did not return is
///    deleted; own rows never are.
/// 4. The sync state records the highest `manifestVersion` applied and the
///    caps this sync ran under. `hub_version` becomes `vouched_version` —
///    the `/me/project-versions` value the version poll acted on — and stays
///    as it is when `None` (ruling R12): the manifest's own `projectVersion`
///    also covers changes only a project refresh picks up (membership, caps,
///    thresholds, dictionary), so it never vouches for anything.
/// 5. `collab-frames-changed` is emitted once per non-zero kind — also for
///    the rows a failed sync applied before it stopped (they are applied
///    locally, and the retry will not see them as changes again).
///
/// Syncs of one project are serialized (a per-project async lock), so a
/// poll-side and a pass-side sync never apply — and emit — the same delta
/// twice. A failure is logged here and leaves the sync state untouched, so
/// the next sync starts from the same cursor; the upserts it already made
/// are idempotent.
pub async fn sync_manifest(
    ctx: &ServiceContext,
    project_id: &str,
    emitter: Option<&dyn ProgressEmitter>,
    vouched_version: Option<i64>,
) -> Result<Vec<CollabFramesChange>, ApiError> {
    let result = sync_manifest_serialized(ctx, project_id, emitter, vouched_version).await;
    if let Err(e) = &result {
        tracing::warn!(
            project_id,
            error = %e,
            "manifest sync failed; the next sync resumes from the stored cursor"
        );
    }
    result
}

/// The lock for one project's manifest syncs, keyed by hub + project.
fn manifest_sync_lock(key: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    > = std::sync::OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Arc::clone(locks.entry(key.to_string()).or_default())
}

/// `<catalog path>|<hub url>|` — the scope of every per-project static in
/// this section, so two app contexts (the e2e harness runs three in one
/// process, against one hub) or two hubs never share state.
fn poll_scope(ctx: &ServiceContext, hub_url: &str) -> Result<String, ApiError> {
    Ok(format!("{}|{hub_url}|", db(ctx)?.path().display()))
}

async fn sync_manifest_serialized(
    ctx: &ServiceContext,
    project_id: &str,
    emitter: Option<&dyn ProgressEmitter>,
    vouched_version: Option<i64>,
) -> Result<Vec<CollabFramesChange>, ApiError> {
    use crate::db::collab_frames as frames_db;

    let Some((hub_url, token)) = crate::api::account::hub_credentials(ctx)? else {
        return Err(ApiError::SignedOut(
            "Sign in to use collaboration projects.".into(),
        ));
    };
    let lock = manifest_sync_lock(&format!("{}{project_id}", poll_scope(ctx, &hub_url)?));
    let _serial = lock.lock().await;

    let project = {
        let database = db(ctx)?;
        let conn = database.conn();
        live_project(&conn, project_id)?
    };
    let client = CollabClient::new(&hub_url).map_err(client_err)?;

    let caps_changed = project.gov_caps_json != project.synced_caps_json;
    let start = if caps_changed {
        0
    } else {
        project.manifest_cursor
    };
    let mut counts: std::collections::BTreeMap<FramesChangeKind, usize> =
        std::collections::BTreeMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut max_mv = project.manifest_cursor;
    let mut applied = 0usize;

    let fetched: Result<(), ApiError> = async {
        let mut since = start;
        let mut after: Option<String> = None;
        loop {
            let page = client
                .manifest_page(
                    &token,
                    project_id,
                    since,
                    after.as_deref(),
                    MANIFEST_PAGE_LIMIT,
                )
                .await
                .map_err(client_err)?;
            {
                let database = db(ctx)?;
                let conn = database.conn();
                let tx = conn.unchecked_transaction()?;
                for v in &page.rows {
                    let prev = frames_db::get(&tx, project_id, &v.frame_uuid)?;
                    for kind in classify_frame_change(prev.as_ref(), v) {
                        *counts.entry(kind).or_default() += 1;
                    }
                    frames_db::upsert_from_manifest(&tx, project_id, v)?;
                    seen.insert(v.frame_uuid.clone());
                    max_mv = max_mv.max(v.manifest_version);
                }
                tx.commit()?;
            }
            applied += page.rows.len();
            if !page.has_more {
                return Ok(());
            }
            match page.next {
                Some(next) => {
                    since = next.since;
                    after = Some(next.after);
                }
                None => {
                    return Err(ApiError::Internal(
                        "hub manifest page has more rows but no cursor".into(),
                    ))
                }
            }
        }
    }
    .await;

    let changes: Vec<CollabFramesChange> = counts
        .into_iter()
        .map(|(kind, count)| CollabFramesChange {
            project_id: project_id.to_string(),
            kind,
            count,
        })
        .collect();
    for change in &changes {
        tracing::info!(
            project_id,
            count = change.count,
            kind = change.kind.as_str(),
            "manifest delta applied"
        );
        if let Some(em) = emitter {
            crate::events::emit_event(em, COLLAB_FRAMES_CHANGED_EVENT, change);
        }
    }
    fetched?;

    let pruned = {
        let database = db(ctx)?;
        let conn = database.conn();
        let pruned = if caps_changed {
            frames_db::delete_not_in(&conn, project_id, &seen)?
        } else {
            0
        };
        crate::db::collab::set_sync_state(
            &conn,
            project_id,
            vouched_version,
            max_mv,
            &project.gov_caps_json,
        )?;
        pruned
    };
    tracing::debug!(
        project_id,
        count = applied,
        pruned,
        caps_changed,
        hub_version = ?vouched_version,
        manifest_cursor = max_mv,
        "manifest synced"
    );
    Ok(changes)
}

/// One version poll (R19).
///
/// - Signed out → `Ok(vec![])`, as the pass does.
/// - `GET /me/project-versions`. A project is a TARGET when it is listed but
///   not cached at that version (moved or joined), or cached and no longer
///   listed (lost). A target in back-off (ruling R13) is skipped. With no
///   target left, that is the whole poll: one request.
/// - Otherwise ONE project refresh limited to the moved projects (a loss is
///   seen from the list), then [`sync_manifest`] for each moved project the
///   refresh actually refreshed, vouching for the listed version.
/// - A project whose refresh or sync fails backs off to the pass cadence
///   ([`COLLAB_AUTO_SYNC_INTERVAL`]) instead of costing hub requests every
///   tick; the back-off is logged when it starts and when it ends. The
///   failure itself was logged where it happened.
/// - [`crate::api::collab::on_thresholds_or_dictionary_moved`] fires for
///   every project whose thresholds or dictionary moved in the refresh,
///   after the syncs.
/// - Returns the moved project ids whose manifest synced (joins included,
///   losses not); the caller kicks the pass when the list is non-empty.
/// - A `collab_api_outdated` refusal is the P17 `Conflict`, logged once.
#[cfg(all(feature = "render", feature = "solver"))]
pub async fn poll_versions_once(
    ctx: &ServiceContext,
    emitter: Option<&dyn ProgressEmitter>,
) -> Result<Vec<String>, ApiError> {
    use std::collections::HashMap;

    let Some((hub_url, token)) = crate::api::account::hub_credentials(ctx)? else {
        tracing::debug!("collab version poll: signed out; skipped");
        return Ok(Vec::new());
    };
    let client = CollabClient::new(&hub_url).map_err(client_err)?;
    let versions = client.project_versions(&token).await.map_err(client_err)?;

    let cached: HashMap<String, i64> = {
        let database = db(ctx)?;
        let conn = database.conn();
        crate::db::collab::list_projects(&conn)?
            .into_iter()
            .map(|r| (r.project_id, r.hub_version))
            .collect()
    };
    let scope = poll_scope(ctx, &hub_url)?;
    let key = |project_id: &str| format!("{scope}{project_id}");
    let listed: HashSet<&str> = versions.iter().map(|v| v.project_id.as_str()).collect();
    let moved_all: Vec<(String, i64)> = versions
        .iter()
        .filter(|v| cached.get(&v.project_id) != Some(&v.version))
        .map(|v| (v.project_id.clone(), v.version))
        .collect();
    let lost_all: Vec<String> = cached
        .keys()
        .filter(|id| !listed.contains(id.as_str()))
        .cloned()
        .collect();
    let moved: Vec<(String, i64)> = moved_all
        .into_iter()
        .filter(|(id, _)| !backoff_active(&key(id)))
        .collect();
    let lost: Vec<String> = lost_all
        .into_iter()
        .filter(|id| !backoff_active(&key(id)))
        .collect();
    if moved.is_empty() && lost.is_empty() {
        tracing::debug!("collab version poll: nothing to do");
        return Ok(Vec::new());
    }
    tracing::debug!(
        count = moved.len(),
        lost = lost.len(),
        "collab version poll: versions moved; refreshing projects"
    );

    let only: HashSet<String> = moved.iter().map(|(id, _)| id.clone()).collect();
    let report = match crate::api::collab::refresh_projects_reporting(ctx, Some(&only)).await {
        Ok(report) => report,
        Err(e) => {
            for id in only.iter().chain(lost.iter()) {
                back_off(&key(id), id, "project refresh failed");
            }
            return Err(e);
        }
    };
    for id in &lost {
        clear_backoff(&key(id), id);
    }

    let mut synced = Vec::new();
    for (project_id, version) in &moved {
        if !report.refreshed.contains(project_id) {
            // Syncing now would vouch for a version whose caps / thresholds /
            // dictionary the cache never saw.
            back_off(&key(project_id), project_id, "project refresh failed");
            continue;
        }
        match sync_manifest(ctx, project_id, emitter, Some(*version)).await {
            Ok(_) => {
                clear_backoff(&key(project_id), project_id);
                synced.push(project_id.clone());
            }
            Err(_) => back_off(&key(project_id), project_id, "manifest sync failed"),
        }
    }
    for project_id in &report.gate_moved {
        crate::api::collab::on_thresholds_or_dictionary_moved(ctx, project_id);
    }
    Ok(synced)
}

/// Tees a [`CollabFramesChange`] event into a caller-supplied `Vec` while
/// still forwarding every event (this one and any other) to the real
/// emitter, if there is one — [`refresh_collab_frames`]'s command wrapper
/// still wants `collab-frames-changed` to reach the frontend live, on top of
/// the plain return value this collects.
struct ChangeCollector<'a> {
    inner: Option<&'a dyn ProgressEmitter>,
    changes: std::sync::Mutex<Vec<CollabFramesChange>>,
}

impl ProgressEmitter for ChangeCollector<'_> {
    fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
        if event_name == COLLAB_FRAMES_CHANGED_EVENT {
            if let Ok(change) = serde_json::from_value::<CollabFramesChange>(payload.clone()) {
                self.changes
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(change);
            }
        }
        if let Some(inner) = self.inner {
            inner.emit_json(event_name, payload);
        }
    }
}

/// Poll every cached project's version (Task 11 command surface): one
/// [`poll_versions_once`] tick, returning every `collab-frames-changed` it
/// applied. The events themselves still reach `emitter` live, exactly as an
/// unattended poll tick would; this only adds the plain return value a manual
/// "refresh" button wants.
#[cfg(all(feature = "render", feature = "solver"))]
pub async fn refresh_collab_frames(
    ctx: &ServiceContext,
    emitter: Option<&dyn ProgressEmitter>,
) -> Result<Vec<CollabFramesChange>, ApiError> {
    let collector = ChangeCollector {
        inner: emitter,
        changes: std::sync::Mutex::new(Vec::new()),
    };
    poll_versions_once(ctx, Some(&collector)).await?;
    Ok(collector
        .changes
        .into_inner()
        .unwrap_or_else(|p| p.into_inner()))
}

/// Per-project back-off (ruling R13): `catalog|hub|project` → the instant before
/// which the version poll leaves the project alone. An entry that has
/// expired stays until a success clears it, so a repeated failure is not
/// logged as a new one.
#[cfg(all(feature = "render", feature = "solver"))]
static POLL_BACKOFF: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
> = std::sync::OnceLock::new();

#[cfg(all(feature = "render", feature = "solver"))]
fn poll_backoff(
) -> std::sync::MutexGuard<'static, std::collections::HashMap<String, std::time::Instant>> {
    POLL_BACKOFF
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Is the poll still leaving this project alone?
#[cfg(all(feature = "render", feature = "solver"))]
fn backoff_active(key: &str) -> bool {
    poll_backoff()
        .get(key)
        .is_some_and(|until| std::time::Instant::now() < *until)
}

/// Back a project off to the pass cadence; logged when the back-off starts.
#[cfg(all(feature = "render", feature = "solver"))]
fn back_off(key: &str, project_id: &str, outcome: &str) {
    let started = poll_backoff()
        .insert(
            key.to_string(),
            std::time::Instant::now() + COLLAB_AUTO_SYNC_INTERVAL,
        )
        .is_none();
    if started {
        tracing::info!(
            project_id,
            outcome,
            retry_secs = COLLAB_AUTO_SYNC_INTERVAL.as_secs(),
            "collab version poll: project backs off to the pass cadence"
        );
    } else {
        tracing::debug!(
            project_id,
            outcome,
            "collab version poll: project still failing"
        );
    }
}

/// End a project's back-off after a success; logged when there was one.
#[cfg(all(feature = "render", feature = "solver"))]
fn clear_backoff(key: &str, project_id: &str) {
    if poll_backoff().remove(key).is_some() {
        tracing::info!(project_id, "collab version poll: project recovered");
    }
}

/// Forget every back-off of one app context (tests: skip the 20-minute wait).
#[cfg(all(test, feature = "render", feature = "solver"))]
pub(crate) fn clear_poll_backoff_for(ctx: &ServiceContext) {
    let prefix = format!("{}|", db(ctx).unwrap().path().display());
    poll_backoff().retain(|k, _| !k.starts_with(&prefix));
}

/// Kick the pass iff the poll saw a version move. Returns whether it kicked
/// (the unit-test oracle). Same stored-permit semantics as
/// [`kick_auto_sync_if_changed`].
#[cfg(all(feature = "render", feature = "solver"))]
fn kick_if_versions_moved(moved: &[String]) -> bool {
    if moved.is_empty() {
        return false;
    }
    auto_sync_kick().notify_one();
    tracing::debug!(
        count = moved.len(),
        "collab auto-sync kicked by a project version move"
    );
    true
}

/// Whether the last whole version poll failed — so a hub outage logs one
/// `warn!` when it starts and one `info!` when it ends, not one per tick.
#[cfg(all(feature = "render", feature = "solver"))]
static POLL_DOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// One tick of the version poll, bound to the worker's context and emitter:
/// poll, then kick the pass when a version moved. Logged at `debug` (it runs
/// 5 760 times a day); a whole-poll failure that starts or ends is logged
/// once at `warn!` / `info!`.
async fn version_poll_tick(ctx: Arc<ServiceContext>, emitter: Option<Arc<dyn ProgressEmitter>>) {
    #[cfg(all(feature = "render", feature = "solver"))]
    {
        use std::sync::atomic::Ordering;
        match poll_versions_once(&ctx, emitter.as_deref()).await {
            Ok(moved) => {
                if POLL_DOWN.swap(false, Ordering::SeqCst) {
                    tracing::info!("collab version poll recovered");
                }
                kick_if_versions_moved(&moved);
            }
            Err(e) => {
                if !POLL_DOWN.swap(true, Ordering::SeqCst) {
                    tracing::warn!(error = %e, "collab version poll failed; retrying every tick");
                } else {
                    tracing::debug!(error = %e, "collab version poll still failing");
                }
            }
        }
    }
    #[cfg(not(all(feature = "render", feature = "solver")))]
    {
        // The project refresh lives in the render+solver-gated `api::collab`;
        // a headless build has no version poll.
        let _ = (ctx, emitter);
    }
}

/// A loop of its own for a periodic job — the version poll (ruling R15) and
/// the maintenance (R18): first tick after `startup_delay`, then every
/// `interval` (a slow tick delays the next instead of bursting). Neither job
/// ever waits for a pass, so a long pass pauses neither.
async fn tick_loop<P, PFut>(
    startup_delay: std::time::Duration,
    interval: std::time::Duration,
    run_poll: P,
) where
    P: Fn() -> PFut,
    PFut: std::future::Future<Output = ()>,
{
    tokio::time::sleep(startup_delay).await;
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        run_poll().await;
    }
}

/// Decode a cached row's retained manifest JSON back into the
/// [`FrameViewWire`](crate::collab::hub_client::FrameViewWire) it was
/// written from (`db::collab_frames::upsert_from_manifest` always writes
/// `serde_json::to_string` of a decoded one) — the shared parse-with-warn
/// every cache-only view built from `project_frames_local` uses for the
/// fields beyond the reliable local columns ([`ProjectFrameView`],
/// `api::collab::list_moderation_queue`'s `ModerationFrameView`). A parse
/// failure never should happen; it is `None` + a `warn!` naming `caller`,
/// never a hard error that would drop the row from its list.
pub(crate) fn parse_manifest_wire(
    project_id: &str,
    frame_uuid: &str,
    manifest_json: &str,
    caller: &str,
) -> Option<crate::collab::hub_client::FrameViewWire> {
    match serde_json::from_str(manifest_json) {
        Ok(w) => Some(w),
        Err(e) => {
            tracing::warn!(
                project_id,
                frame_uuid,
                error = %e,
                caller,
                "manifest_json did not parse — some fields omitted"
            );
            None
        }
    }
}

/// One cached per-frame manifest row of a project (mine or a peer's),
/// projected for the frames list (wave 2 Task 11).
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ProjectFrameView {
    pub frame_uuid: String,
    pub file_name: String,
    pub publisher: String,
    /// I published this frame.
    pub own: bool,
    pub filter: String,
    pub exptime_sec: f64,
    pub date_obs: Option<String>,
    /// Hub-mirrored state: `pending` | `published` | `rejected`.
    pub state: String,
    pub accepted: bool,
    pub accepted_reason: Option<String>,
    /// Holders the hub last reported.
    pub holder_count: i64,
    pub on_disk: bool,
    /// A newer content version superseded this landed copy — kept until GC.
    pub awaiting_gc: bool,
    /// This device chose not to keep the frame (policy narrowed, or the loss
    /// guard's "stop holding" answer).
    pub locally_declined: bool,
    pub byte_size: i64,
    pub content_version: i32,
    pub last_error: Option<String>,
    /// Parsed from the manifest row's `meta.fwhmArcsec` (`build_frame_meta`).
    pub fwhm_arcsec: Option<f64>,
    /// Parsed from `meta.eccentricity`.
    pub eccentricity: Option<f64>,
    /// Parsed from `meta.starsDetected`.
    pub stars_detected: Option<i64>,
}

impl ProjectFrameView {
    /// Combines the reliable local columns (state/accepted/holder_count/
    /// on_disk/… — kept current by the manifest sync and the replication
    /// pass) with the fields only the retained manifest row carries
    /// (exptime/dateObs/acceptedReason/meta metrics). A row whose
    /// `manifest_json` fails to parse (it never should — this cache only
    /// ever writes it via `serde_json::to_string` of a decoded
    /// [`FrameViewWire`]) still returns a view, with those fields empty and
    /// a `warn!` — never a lost frame from the list.
    fn from_local_row(row: LocalFrameRow) -> Self {
        let wire = parse_manifest_wire(
            &row.project_id,
            &row.frame_uuid,
            &row.manifest_json,
            "list_project_frames",
        );
        let (exptime_sec, date_obs, accepted_reason, fwhm_arcsec, eccentricity, stars_detected) =
            match &wire {
                Some(w) => (
                    w.exptime_sec,
                    w.date_obs.clone(),
                    w.accepted_reason.clone(),
                    w.meta.get("fwhmArcsec").and_then(serde_json::Value::as_f64),
                    w.meta
                        .get("eccentricity")
                        .and_then(serde_json::Value::as_f64),
                    w.meta
                        .get("starsDetected")
                        .and_then(serde_json::Value::as_i64),
                ),
                None => (0.0, None, None, None, None, None),
            };
        ProjectFrameView {
            frame_uuid: row.frame_uuid,
            file_name: row.file_name,
            publisher: row.publisher_display,
            own: row.origin == FrameOrigin::Own,
            filter: row.filter_canonical,
            exptime_sec,
            date_obs,
            state: row.state,
            accepted: row.accepted,
            accepted_reason,
            holder_count: row.holder_count,
            on_disk: row.on_disk,
            awaiting_gc: row.awaiting_gc,
            locally_declined: row.locally_declined,
            byte_size: row.byte_size,
            content_version: row.content_version,
            last_error: row.last_error,
            fwhm_arcsec,
            eccentricity,
            stars_detected,
        }
    }
}

/// Every cached frame of a project (cache-only — no hub call), ordered by
/// frame uuid (the `list_for_project` order). The manifest sync (Task 8) and
/// the replication pass (Task 9) keep the cache current; this never fetches.
pub fn list_project_frames(
    ctx: &ServiceContext,
    project_id: &str,
) -> Result<Vec<ProjectFrameView>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let rows = crate::db::collab_frames::list_for_project(&conn, project_id)?;
    Ok(rows
        .into_iter()
        .map(ProjectFrameView::from_local_row)
        .collect())
}

/// Stop seeding EVERY frame of one project — the site where this device stops
/// being a member of the project at all (R14). Drops the project's tags from
/// both stores ([`unseed_project`](crate::sharing::iroh::node::SharedIrohNode::unseed_project));
/// the files are never touched.
pub async fn unseed_project_local_data(ctx: &ServiceContext, project_id: &str) {
    let Some(node) = ctx.iroh_node.lock().await.clone() else {
        tracing::debug!(project_id, "unseed skipped: no iroh node bound");
        return;
    };
    node.unseed_project(project_id).await;
}

// ── Collab v3 wave 2: per-frame replication (Task 9) ──────────────────────────
//
// The pass below turns the manifest (Task 8) into files on disk and the disk
// back into holder reports:
//
// - disk truth: every landed row's file is stat'ed (and re-hashed when its
//   `size:mtime` moved); a file that is gone or edited is MISSING — its seed
//   tags are dropped and the hub stops sending peers here (spec §5.5, P8);
// - the loss guard (P14) pauses replication when "missing" looks like a
//   folder moved away rather than a frame deleted;
// - the need set (spec §5.3) is every published, accepted peer frame not on
//   disk, filtered by the project's replication policy, rarest first;
// - the fetch pulls a batch through the generalized assignment engine (P11)
//   into the collab store and lands each frame by `export_child`, i.e. by
//   renaming the store's data file to its final path (P21): the landed file
//   IS the seed.

/// The message every publish and replication path refuses with when no
/// Collaboration folder is set (collab v3 wave 2, P25). No silent fallback to
/// the working dir.
pub(crate) const COLLABORATION_ROOT_REQUIRED: &str =
    "set a Collaboration folder in File Manager → Folders first";

/// The configured Collaboration root — required for any collab receive or
/// publish (P25). Absent ⇒ `ApiError::Invalid` carrying
/// [`COLLABORATION_ROOT_REQUIRED`], logged at `warn!`.
pub(crate) fn require_collaboration_root(ctx: &ServiceContext) -> Result<PathBuf, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    match crate::db::scan_root_path_of_kind(&conn, "collaboration") {
        Ok(Some(path)) => Ok(PathBuf::from(path)),
        Ok(None) => {
            tracing::warn!(
                outcome = "no_collaboration_root",
                "collaboration root required"
            );
            Err(ApiError::Invalid(COLLABORATION_ROOT_REQUIRED.to_string()))
        }
        Err(e) => {
            tracing::error!(error = %e, "read collaboration root failed");
            Err(ApiError::Internal(format!("read collaboration root: {e}")))
        }
    }
}

/// A publisher's folder in a project (P10): the folder that publisher's rows
/// already use, else `<Collab>/<project slug>/<display name>`, sanitized and
/// made unique against every other publisher's folder in the project.
/// `fallback` names the folder when the display name is empty. Shared by
/// publish (my own folder) and landing (a peer's folder). A rename of the
/// account never moves files: the folder is found by account id.
pub(crate) fn publisher_folder(
    conn: &rusqlite::Connection,
    collab_root: &Path,
    project: &crate::db::collab::CollabProjectRow,
    account_id: &str,
    display: &str,
    fallback: &str,
) -> Result<PathBuf> {
    if !account_id.is_empty() {
        if let Some(dir) =
            crate::db::collab_frames::publisher_dir(conn, &project.project_id, account_id)?
        {
            return Ok(dir);
        }
    }
    let project_dir = collab_root.join(crate::sync::ingest::sanitize_slug(&project.slug));
    let taken: HashSet<PathBuf> = {
        let mut stmt = conn.prepare(
            "SELECT landed_path FROM project_frames_local
             WHERE project_id = ?1 AND publisher_account_id != ?2 AND landed_path IS NOT NULL",
        )?;
        let paths = stmt
            .query_map(rusqlite::params![project.project_id, account_id], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        paths
            .into_iter()
            .filter_map(|p| Path::new(&p).parent().map(Path::to_path_buf))
            .collect()
    };
    let base = crate::sync::ingest::sanitize_slug(if display.is_empty() {
        fallback
    } else {
        display
    });
    for n in 1..10_000 {
        let name = if n == 1 {
            base.clone()
        } else {
            format!("{base}_{n}")
        };
        let candidate = project_dir.join(name);
        if !taken.contains(&candidate) {
            return Ok(candidate);
        }
    }
    anyhow::bail!(
        "no free publisher folder name under {}",
        project_dir.display()
    )
}

/// Emitted once when the loss guard pauses a project's replication
/// ([`CollabReplicationPaused`], P14).
pub const COLLAB_REPLICATION_PAUSED_EVENT: &str = "collab-replication-paused";

/// Emitted once per replication fetch that landed or failed anything
/// ([`CollabFramesLanded`]) — a discrete outcome, never progress.
pub const COLLAB_FRAMES_LANDED_EVENT: &str = "collab-frames-landed";

/// The hub's cap on `add` (and `remove`) per `PUT …/holders/self` (P8).
const HOLDERS_CHUNK: usize = 10_000;

/// Frames WITH providers per assignment run (ruling R16: a frame no holder
/// can serve never takes a slot). Batches run back to back inside one fetch
/// until the need set is empty or a batch that attempted fetches landed
/// nothing.
const FETCH_BATCH: usize = 200;

/// Which replication pass is running (spec §5.5, rulings R15/R18).
///
/// - `Fetch` — the pass loop, on its timer (the retry cadence) or on a kick
///   (a version moved; the 15 s poll already applied the manifest): need
///   set, fetch, land, holder delta. Never a stat walk, never a full report —
///   those run on their own loop ([`run_maintenance`]), so a multi-hour fetch
///   cannot starve them past the hub's 75-minute holder freshness.
/// - `Forced` — "Sync now": [`run_maintenance`] for the project first, then
///   the fetch, with the auto-replicate toggle forced on (the role gate
///   never is).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PassKind {
    Fetch,
    Forced,
}

impl PassKind {
    fn as_str(self) -> &'static str {
        match self {
            PassKind::Fetch => "fetch",
            PassKind::Forced => "forced",
        }
    }
}

/// A project's local replication policy (spec §5.3). Every constraint is
/// optional; empty lists mean "all". Stored as JSON in
/// `collab_projects.policy_json`, never sent to the hub.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ReplicationPolicy {
    /// Canonical filters to replicate (the dictionary's `canonical`).
    #[serde(default)]
    pub filters: Vec<String>,
    /// Publisher account ids to replicate.
    #[serde(default)]
    pub publishers: Vec<String>,
    /// Only frames whose manifest `meta.fwhmArcsec` is at most this. A frame
    /// without the measurement does not match a set bound.
    #[serde(default)]
    pub max_fwhm_arcsec: Option<f64>,
    /// Only frames whose manifest `meta.starsDetected` is at least this. A
    /// frame without the measurement does not match a set bound.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub min_stars: Option<i64>,
    /// At most this many bytes of this project's replicas on disk, counting
    /// the ones already held.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub byte_budget: Option<i64>,
}

/// What a policy selects in a project, as of the local cache
/// (`preview_collab_policy` / `set_collab_policy`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PolicyPreview {
    /// Published, accepted, not-declined peer frames the policy matches.
    pub frames: usize,
    #[ts(type = "number")]
    pub bytes: i64,
    /// Of those, already on disk.
    pub already_held: usize,
    /// What the next pass would fetch (byte budget applied).
    pub to_fetch: usize,
    #[ts(type = "number")]
    pub to_fetch_bytes: i64,
}

/// Payload of [`COLLAB_REPLICATION_PAUSED_EVENT`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CollabReplicationPaused {
    pub project_id: String,
    pub missing: usize,
    #[ts(type = "number")]
    pub missing_bytes: i64,
}

/// Payload of [`COLLAB_FRAMES_LANDED_EVENT`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CollabFramesLanded {
    pub project_id: String,
    pub landed: usize,
    pub failed: usize,
    pub awaiting_gc: usize,
}

/// The user's answer to a paused project (P14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum LossAction {
    /// Rescan the Collaboration root (the scanner repairs moved files), run
    /// disk truth again, unpause.
    Restore,
    /// Stop holding the missing frames (`locally_declined`), unpause.
    StopHolding,
}

/// What one disk-truth walk found (spec §5.5).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DiskTruth {
    /// `(frame_uuid, content_version)` of every frame whose file is on disk
    /// with its recorded content — the full holder report's source (P8).
    pub present: Vec<(String, i32)>,
    /// Peer frames whose landed file vanished or was edited this pass.
    pub missing_replicas: Vec<String>,
    /// My own frames whose landed file vanished or was edited this pass.
    pub missing_own: Vec<String>,
    /// Bytes of `missing_replicas`.
    pub missing_bytes: i64,
    /// Files re-hashed because their `size:mtime` moved.
    pub rehashed: usize,
    /// Replicas that were on disk when the walk started (the loss guard's
    /// denominator).
    pub held_replicas: usize,
    /// Present frames parked because a byte-identical sibling went missing
    /// and took their shared store entry with it (P20/P24): their file is
    /// fine, but the store cannot serve it until GC drops the dead entry.
    pub parked: Vec<String>,
}

/// What one [`fetch_frames`] call did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FetchOutcome {
    pub landed: usize,
    pub failed: usize,
    pub awaiting_gc: usize,
}

impl std::ops::AddAssign for FetchOutcome {
    fn add_assign(&mut self, o: Self) {
        self.landed += o.landed;
        self.failed += o.failed;
        self.awaiting_gc += o.awaiting_gc;
    }
}

/// `"size:mtime_secs"` — the same spelling publish records.
fn size_mtime_from(meta: &std::fs::Metadata) -> String {
    let secs = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{}:{secs}", meta.len())
}

/// A string field of a row's verbatim manifest JSON.
fn manifest_str(row: &LocalFrameRow, key: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(&row.manifest_json)
        .ok()?
        .get(key)?
        .as_str()
        .map(str::to_string)
}

/// A numeric `meta` field of a row's verbatim manifest JSON.
fn manifest_meta_f64(row: &LocalFrameRow, key: &str) -> Option<f64> {
    serde_json::from_str::<serde_json::Value>(&row.manifest_json)
        .ok()?
        .get("meta")?
        .get(key)?
        .as_f64()
}

/// Does `row` pass the policy's filter / publisher / quality constraints?
fn policy_matches(row: &LocalFrameRow, policy: &ReplicationPolicy) -> bool {
    if !policy.filters.is_empty() && !policy.filters.contains(&row.filter_canonical) {
        return false;
    }
    if !policy.publishers.is_empty() && !policy.publishers.contains(&row.publisher_account_id) {
        return false;
    }
    if let Some(max) = policy.max_fwhm_arcsec {
        match manifest_meta_f64(row, "fwhmArcsec") {
            Some(fwhm) if fwhm <= max => {}
            _ => return false,
        }
    }
    if let Some(min) = policy.min_stars {
        match manifest_meta_f64(row, "starsDetected") {
            Some(stars) if stars >= min as f64 => {}
            _ => return false,
        }
    }
    true
}

/// Is `row` a peer frame this device may hold at all (before policy)?
fn replicable(row: &LocalFrameRow) -> bool {
    row.state == "published"
        && row.accepted
        && row.origin == FrameOrigin::Replica
        && !row.locally_declined
}

/// The need set (spec §5.3), pure: which rows to fetch now.
///
/// `published ∧ accepted ∧ replica ∧ ¬on_disk ∧ ¬locally_declined ∧
/// ¬awaiting_gc ∧ policy`, empty when the role forbids replication, the
/// toggle is off, or the loss guard paused the project. Ordered rarest first
/// (`holder_count`), then oldest (`createdAt`), then by uuid. A byte budget
/// counts the replicas already on disk and stops at the first frame that
/// would cross it.
pub(crate) fn frame_need(
    rows: &[LocalFrameRow],
    policy: &ReplicationPolicy,
    role_allows: bool,
    auto_on: bool,
    paused: bool,
) -> Vec<LocalFrameRow> {
    if !role_allows || !auto_on || paused {
        return Vec::new();
    }
    let mut need: Vec<(i64, String, &LocalFrameRow)> = rows
        .iter()
        .filter(|r| replicable(r) && !r.on_disk && !r.awaiting_gc && policy_matches(r, policy))
        .map(|r| {
            (
                r.holder_count,
                manifest_str(r, "createdAt").unwrap_or_default(),
                r,
            )
        })
        .collect();
    need.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.frame_uuid.cmp(&b.2.frame_uuid))
    });
    let mut out = Vec::with_capacity(need.len());
    match policy.byte_budget {
        None => out.extend(need.into_iter().map(|(_, _, r)| r.clone())),
        Some(budget) => {
            let mut held: i64 = rows
                .iter()
                .filter(|r| r.origin == FrameOrigin::Replica && r.on_disk)
                .map(|r| r.byte_size)
                .sum();
            for (_, _, r) in need {
                if held.saturating_add(r.byte_size) > budget {
                    break;
                }
                held += r.byte_size;
                out.push(r.clone());
            }
        }
    }
    out
}

/// The node bound on the context, if any (never binds one).
async fn bound_node(
    ctx: &ServiceContext,
) -> Option<Arc<crate::sharing::iroh::node::SharedIrohNode>> {
    ctx.iroh_node.lock().await.clone()
}

/// The health of a row's content in the collab store. `None` when no node or
/// no collab store is there to ask, or the hash does not parse (logged).
async fn frame_health(
    node: Option<&Arc<crate::sharing::iroh::node::SharedIrohNode>>,
    row: &LocalFrameRow,
) -> Option<crate::sharing::iroh::node::BlobHealth> {
    let node = node?;
    node.collab_store()?;
    let hash = match row.blake3.parse::<iroh_blobs::Hash>() {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(frame_uuid = %row.frame_uuid, error = %e, "frame blake3 does not parse");
            return None;
        }
    };
    node.collab_blob_health(hash).await.ok()
}

/// Full-file xxh3 on a blocking thread.
async fn xxh3_on_blocking(path: &Path) -> Result<String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || crate::package::xxh3_full_file(&path))
        .await
        .context("xxh3 task join")?
}

/// Drop one frame's seed tags when a node with a collab store is bound.
/// Failures are logged inside [`unseed_project_frame`](crate::sharing::iroh::node::SharedIrohNode::unseed_project_frame).
async fn unseed_frame(
    node: Option<&Arc<crate::sharing::iroh::node::SharedIrohNode>>,
    project_id: &str,
    frame_uuid: &str,
) {
    if let Some(node) = node {
        if node.collab_store().is_some() {
            let _ = node.unseed_project_frame(project_id, frame_uuid).await;
        }
    }
}

/// The per-project lock between disk truth / the parked-frame recheck / the
/// in-flight sweep on one side and a landing on the other (ruling R18). Disk
/// truth holds it per row, a landing per frame, and nobody ever holds it
/// across a network fetch — so the maintenance loop and a multi-hour fetch
/// interleave frame by frame. Keyed by catalog + project.
fn project_disk_lock(
    ctx: &ServiceContext,
    project_id: &str,
) -> Result<Arc<tokio::sync::Mutex<()>>, ApiError> {
    static LOCKS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    > = std::sync::OnceLock::new();
    let key = format!("{}|{project_id}", db(ctx)?.path().display());
    let mut locks = LOCKS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Ok(Arc::clone(locks.entry(key).or_default()))
}

/// The designated Collaboration root, without the "required" warning (a
/// caller that already required it, or one where it is optional).
fn collaboration_root_quiet(ctx: &ServiceContext) -> Option<PathBuf> {
    let db = db(ctx).ok()?;
    let conn = db.conn();
    match crate::db::scan_root_path_of_kind(&conn, "collaboration") {
        Ok(p) => p.map(PathBuf::from),
        Err(e) => {
            tracing::warn!(error = %e, "read collaboration root failed");
            None
        }
    }
}

/// The Collaboration root, required AND present on disk (m5). A root on an
/// unmounted volume answers `None` with one `warn!`, so a pass never counts
/// every landed file as missing, and never lands into a phantom folder.
fn mounted_collaboration_root(ctx: &ServiceContext) -> Option<PathBuf> {
    let root = require_collaboration_root(ctx).ok()?;
    if root.is_dir() {
        Some(root)
    } else {
        tracing::warn!(
            path = %root.display(),
            outcome = "collaboration_root_unavailable",
            "collaboration folder is not reachable; replication skipped"
        );
        None
    }
}

/// A replica's landed path must lie under the current Collaboration root;
/// one outside it (a root that moved) counts as missing (m5). Own frames may
/// live anywhere (P26).
fn inside_root(root: Option<&Path>, row: &LocalFrameRow, path: &Path) -> bool {
    row.origin == FrameOrigin::Own || root.is_none_or(|r| path.starts_with(r))
}

/// Disk truth for one project (spec §5.5). For every row with a landed path,
/// re-read under the project's disk lock (R18):
///
/// - on disk: `stat` it; gone (or a replica outside the Collaboration root)
///   → MISSING. A `size:mtime` that moved → re-hash; a different xxh3 is an
///   edit and counts as MISSING, the same one stores the new `size:mtime`. A
///   missing frame is marked (`on_disk = 0`), its seed tags are dropped, and
///   a replica's `awaiting_gc` records whether its store entry is already
///   gone (`Missing`) or must be collected first (anything else, P20 — never
///   fetch over a live or dead entry).
/// - not on disk (lost earlier, or put back by a rescan): when the file at
///   the path has the recorded size and xxh3 and the store can take it
///   (entry `Missing` or `Readable`), it is seeded again and re-admitted. A
///   file whose content was rejected is not hashed again while its
///   `size:mtime` stays the same (R21: a version-bumped replica's old file).
///
/// A present frame whose byte-identical sibling went missing may have lost
/// its readable path with it (the store reads one path per entry, P24); such
/// a frame is parked (`awaiting_gc`) until GC drops the dead entry.
pub(crate) async fn disk_truth(
    ctx: &ServiceContext,
    project_id: &str,
) -> Result<DiskTruth, ApiError> {
    let uuids: Vec<String> = {
        let db = db(ctx)?;
        let conn = db.conn();
        crate::db::collab_frames::list_for_project(&conn, project_id)?
            .into_iter()
            .filter(|r| r.landed_path.is_some())
            .map(|r| r.frame_uuid)
            .collect()
    };
    let node = bound_node(ctx).await;
    let root = collaboration_root_quiet(ctx);
    let lock = project_disk_lock(ctx, project_id)?;
    let mut truth = DiskTruth::default();
    let mut lost_hashes: HashSet<String> = HashSet::new();
    let mut present_rows: Vec<LocalFrameRow> = Vec::new();

    for uuid in uuids {
        let _guard = lock.lock().await;
        let row = {
            let db = db(ctx)?;
            let conn = db.conn();
            crate::db::collab_frames::get(&conn, project_id, &uuid)?
        };
        let Some(row) = row else { continue };
        let Some(landed) = row.landed_path.clone() else {
            continue;
        };
        let path = Path::new(&landed);
        if !row.on_disk {
            if row.locally_declined || !inside_root(root.as_deref(), &row, path) {
                continue;
            }
            if readmit(ctx, node.as_ref(), &row, path, &mut truth.rehashed).await? {
                tracing::info!(project_id, frame_uuid = %row.frame_uuid, path = %landed, "frame re-admitted from disk");
                truth
                    .present
                    .push((row.frame_uuid.clone(), row.content_version));
                present_rows.push(row);
            }
            continue;
        }
        if row.origin == FrameOrigin::Replica {
            truth.held_replicas += 1;
        }
        let present = if !inside_root(root.as_deref(), &row, path) {
            tracing::warn!(project_id, frame_uuid = %row.frame_uuid, path = %landed, "landed replica lies outside the collaboration folder; counted missing");
            false
        } else {
            match tokio::fs::metadata(path).await {
                Ok(meta) if meta.is_file() => {
                    let sm = size_mtime_from(&meta);
                    if row.size_mtime_seen.as_deref() == Some(sm.as_str()) {
                        true
                    } else {
                        truth.rehashed += 1;
                        match xxh3_on_blocking(path).await {
                            Ok(h) if h == row.xxh3 => {
                                let db = db(ctx)?;
                                crate::db::collab_frames::set_size_mtime_seen(
                                    &db.conn(),
                                    project_id,
                                    &row.frame_uuid,
                                    &sm,
                                )?;
                                true
                            }
                            Ok(h) => {
                                tracing::warn!(project_id, frame_uuid = %row.frame_uuid, path = %landed, xxh3 = %h, "landed frame was edited; counted missing");
                                false
                            }
                            Err(e) => {
                                tracing::warn!(project_id, frame_uuid = %row.frame_uuid, path = %landed, error = %format!("{e:#}"), "re-hash of a landed frame failed; counted missing");
                                false
                            }
                        }
                    }
                }
                Ok(_) => {
                    tracing::warn!(project_id, frame_uuid = %row.frame_uuid, path = %landed, "landed frame path is not a file");
                    false
                }
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        tracing::debug!(project_id, frame_uuid = %row.frame_uuid, path = %landed, "landed frame is gone");
                    } else {
                        tracing::warn!(project_id, frame_uuid = %row.frame_uuid, path = %landed, error = %e, "stat of a landed frame failed; counted missing");
                    }
                    false
                }
            }
        };
        if present {
            truth
                .present
                .push((row.frame_uuid.clone(), row.content_version));
            present_rows.push(row);
            continue;
        }

        // Missing. Conservative first: `awaiting_gc` until the store says
        // the entry is gone.
        {
            let db = db(ctx)?;
            crate::db::collab_frames::set_missing(
                &db.conn(),
                project_id,
                &row.frame_uuid,
                row.origin == FrameOrigin::Replica,
            )?;
        }
        unseed_frame(node.as_ref(), project_id, &row.frame_uuid).await;
        lost_hashes.insert(row.blake3.clone());
        match row.origin {
            FrameOrigin::Own => truth.missing_own.push(row.frame_uuid.clone()),
            FrameOrigin::Replica => {
                if frame_health(node.as_ref(), &row).await
                    == Some(crate::sharing::iroh::node::BlobHealth::Missing)
                {
                    let db = db(ctx)?;
                    crate::db::collab_frames::set_missing(
                        &db.conn(),
                        project_id,
                        &row.frame_uuid,
                        false,
                    )?;
                }
                truth.missing_replicas.push(row.frame_uuid.clone());
                truth.missing_bytes = truth.missing_bytes.saturating_add(row.byte_size);
            }
        }
    }

    // P24: a present frame that shares its content with a lost one.
    for row in present_rows {
        if !lost_hashes.contains(&row.blake3) {
            continue;
        }
        let _guard = lock.lock().await;
        let row = {
            let db = db(ctx)?;
            let fresh = crate::db::collab_frames::get(&db.conn(), project_id, &row.frame_uuid)?;
            match fresh {
                Some(f) if f.on_disk && f.blake3 == row.blake3 => f,
                _ => continue,
            }
        };
        if frame_health(node.as_ref(), &row).await
            != Some(crate::sharing::iroh::node::BlobHealth::Dead)
        {
            continue;
        }
        tracing::warn!(project_id, frame_uuid = %row.frame_uuid, "identical frame lost its readable store path; parked until GC");
        {
            let db = db(ctx)?;
            crate::db::collab_frames::set_missing(&db.conn(), project_id, &row.frame_uuid, true)?;
        }
        unseed_frame(node.as_ref(), project_id, &row.frame_uuid).await;
        truth.present.retain(|(u, _)| u != &row.frame_uuid);
        truth.parked.push(row.frame_uuid.clone());
    }

    tracing::debug!(
        project_id,
        count = truth.present.len(),
        missing = truth.missing_replicas.len() + truth.missing_own.len(),
        rehashed = truth.rehashed,
        "disk truth"
    );
    Ok(truth)
}

/// Re-admit a frame whose file is back at its landed path (a rescan's
/// "moved" repair, a restored folder): right size, right xxh3, and a store
/// that can take it (entry `Missing` or `Readable` — never over a `Dead` or
/// `Partial` one, P20). Seeds it and marks it landed. `true` when
/// re-admitted. A rejected file's `size:mtime` is remembered so an unchanged
/// file is not hashed again (R21); `rehashed` counts the hashes taken.
async fn readmit(
    ctx: &ServiceContext,
    node: Option<&Arc<crate::sharing::iroh::node::SharedIrohNode>>,
    row: &LocalFrameRow,
    path: &Path,
    rehashed: &mut usize,
) -> Result<bool, ApiError> {
    use crate::sharing::iroh::node::BlobHealth;
    let Ok(meta) = tokio::fs::metadata(path).await else {
        return Ok(false);
    };
    if !meta.is_file() || meta.len() as i64 != row.byte_size {
        return Ok(false);
    }
    let sm = size_mtime_from(&meta);
    let rejected = {
        let db = db(ctx)?;
        crate::db::collab_frames::rejected_size_mtime(&db.conn(), &row.project_id, &row.frame_uuid)?
    };
    if rejected.as_deref() == Some(sm.as_str()) {
        return Ok(false);
    }
    let Some(node) = node else {
        return Ok(false);
    };
    if !matches!(
        frame_health(Some(node), row).await,
        Some(BlobHealth::Missing | BlobHealth::Readable)
    ) {
        return Ok(false);
    }
    *rehashed += 1;
    match xxh3_on_blocking(path).await {
        Ok(h) if h == row.xxh3 => {}
        Ok(_) => {
            let db = db(ctx)?;
            crate::db::collab_frames::set_rejected_size_mtime(
                &db.conn(),
                &row.project_id,
                &row.frame_uuid,
                &sm,
            )?;
            return Ok(false);
        }
        Err(e) => {
            tracing::debug!(frame_uuid = %row.frame_uuid, error = %format!("{e:#}"), "re-admission hash failed");
            return Ok(false);
        }
    }
    if node
        .seed_project_frame(&row.project_id, &row.frame_uuid, row.content_version, path)
        .await
        .is_err()
    {
        // Logged inside; a Dead entry is parked by the next pass.
        return Ok(false);
    }
    let db = db(ctx)?;
    crate::db::collab_frames::set_landed(
        &db.conn(),
        &row.project_id,
        &row.frame_uuid,
        &path.to_string_lossy(),
        &sm,
    )?;
    Ok(true)
}

/// Clear `awaiting_gc` on every parked replica whose store entry is gone
/// (`Missing`) or only partial — the need set may fetch it again (P20). Part
/// of maintenance only (m2).
async fn recheck_awaiting_gc(ctx: &ServiceContext, project_id: &str) -> Result<usize, ApiError> {
    use crate::sharing::iroh::node::BlobHealth;
    let node = bound_node(ctx).await;
    if node.as_ref().and_then(|n| n.collab_store()).is_none() {
        return Ok(0);
    }
    let lock = project_disk_lock(ctx, project_id)?;
    let _guard = lock.lock().await;
    let parked: Vec<LocalFrameRow> = {
        let db = db(ctx)?;
        let conn = db.conn();
        crate::db::collab_frames::list_for_project(&conn, project_id)?
            .into_iter()
            .filter(|r| r.awaiting_gc && !r.on_disk)
            .collect()
    };
    let mut cleared = 0;
    for row in &parked {
        if matches!(
            frame_health(node.as_ref(), row).await,
            Some(BlobHealth::Missing | BlobHealth::Partial)
        ) {
            let db = db(ctx)?;
            crate::db::collab_frames::set_missing(&db.conn(), project_id, &row.frame_uuid, false)?;
            cleared += 1;
        }
    }
    if cleared > 0 {
        tracing::info!(
            project_id,
            count = cleared,
            "collected frames may be fetched again"
        );
    }
    Ok(cleared)
}

/// Would a fetch of this project take `row` at `version`, pause, toggle and
/// budget aside? The in-flight sweep's keep rule (R23).
fn still_wanted(row: &LocalFrameRow, version: i32, policy: &ReplicationPolicy) -> bool {
    replicable(row)
        && !row.on_disk
        && !row.awaiting_gc
        && row.content_version == version
        && policy_matches(row, policy)
}

/// Delete the project's `in-flight/project/<pid>/<uuid>/<ver>` tags whose
/// frame is no longer wanted (R23): a transfer failure keeps its tag so the
/// next fetch resumes from the verified partial bytes, and this sweep is what
/// lets GC have them once the frame leaves the need set. Holds the project's
/// fetch claim throughout, and is skipped while a fetch of the project runs —
/// its tags are live. Returns the tags removed.
async fn sweep_in_flight(ctx: &ServiceContext, project_id: &str) -> Result<usize, ApiError> {
    use n0_future::StreamExt as _;
    let Some(store) = bound_node(ctx).await.and_then(|n| n.collab_store()) else {
        return Ok(0);
    };
    // N4: hold the project's fetch claim for the whole sweep, so no fetch can
    // set a tag this sweep would judge stale.
    let Some(_claim) = FramePullClaim::acquire(&fetch_key(ctx, project_id)?) else {
        return Ok(0);
    };
    let lock = project_disk_lock(ctx, project_id)?;
    let _guard = lock.lock().await;
    let (rows, policy) = {
        let db = db(ctx)?;
        let conn = db.conn();
        let project = live_project(&conn, project_id)?;
        let rows: std::collections::HashMap<String, LocalFrameRow> =
            crate::db::collab_frames::list_for_project(&conn, project_id)?
                .into_iter()
                .map(|r| (r.frame_uuid.clone(), r))
                .collect();
        (rows, read_policy(&project))
    };
    let prefix = crate::sharing::iroh::blobs::in_flight_tag(&format!("project/{project_id}/"));
    let mut stale: Vec<String> = Vec::new();
    let mut stream = store
        .tags()
        .list_prefix(prefix.as_bytes())
        .await
        .map_err(|e| ApiError::Internal(format!("list in-flight tags: {e}")))?;
    while let Some(item) = stream.next().await {
        let info = match item {
            Ok(info) => info,
            Err(e) => {
                tracing::warn!(project_id, error = %e, "list in-flight tags failed");
                return Ok(0);
            }
        };
        let name = String::from_utf8_lossy(info.name.as_ref()).to_string();
        let keep = name
            .strip_prefix(&prefix)
            .and_then(|rest| rest.split_once('/'))
            .and_then(|(uuid, ver)| Some((rows.get(uuid)?, ver.parse::<i32>().ok()?)))
            .is_some_and(|(row, ver)| still_wanted(row, ver, &policy));
        if !keep {
            stale.push(name);
        }
    }
    for tag in &stale {
        drop_tag(&store, tag).await;
    }
    if !stale.is_empty() {
        tracing::info!(
            project_id,
            count = stale.len(),
            "stale in-flight tags swept"
        );
    }
    Ok(stale.len())
}

/// What one maintenance run did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct MaintenanceOutcome {
    /// Projects walked.
    pub projects: usize,
    /// Full holder reports sent.
    pub reported: usize,
    /// Replicas found missing (fetchable once their entry is gone).
    pub missing: usize,
    /// Parked frames made fetchable again.
    pub cleared: usize,
}

/// The maintenance of every live project (or `scope`), on its own loop
/// (ruling R18) and before a "Sync now" fetch: [`disk_truth`], the
/// [`loss_guard`] (skipped when already paused), the FULL
/// [`report_holders`] — for every role, a `send` member still holds its own
/// frames — then the parked-frame recheck (P20) and the in-flight sweep
/// (R23). Signed out, no Collaboration root, or a root that is not reachable
/// ⇒ nothing (one `warn!`). Never returns an error: each project's failure
/// is logged and stepped over.
pub(crate) async fn run_maintenance(
    ctx: &ServiceContext,
    scope: Option<&str>,
    emitter: Option<&dyn ProgressEmitter>,
) -> MaintenanceOutcome {
    let mut outcome = MaintenanceOutcome::default();
    match crate::api::account::hub_credentials(ctx) {
        Ok(Some(_)) => {}
        Ok(None) => return outcome,
        Err(e) => {
            tracing::warn!(error = %format!("{e}"), "collab maintenance: account read failed; skipped");
            return outcome;
        }
    }
    if mounted_collaboration_root(ctx).is_none() {
        return outcome;
    }
    let projects = match db(ctx).and_then(|d| Ok(crate::db::collab::list_projects(&d.conn())?)) {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "collab maintenance: project list failed; skipped");
            return outcome;
        }
    };
    for project in projects {
        if scope.is_some_and(|only| only != project.project_id) {
            continue;
        }
        let pid = project.project_id.as_str();
        outcome.projects += 1;
        let truth = match disk_truth(ctx, pid).await {
            Ok(truth) => truth,
            Err(e) => {
                tracing::warn!(project_id = pid, error = %e, "collab maintenance: disk truth failed; project skipped");
                continue;
            }
        };
        outcome.missing += truth.missing_replicas.len();
        if !project.replication_paused {
            if let Err(e) = loss_guard(ctx, pid, &truth, emitter) {
                tracing::warn!(project_id = pid, error = %e, "collab maintenance: loss guard failed");
            }
        }
        // The truth goes to the hub either way — a paused project still stops
        // advertising what it lost.
        // N2: what is on disk NOW — a frame landed while the walk ran is
        // held too.
        let present = match db(ctx)
            .and_then(|d| Ok(crate::db::collab_frames::list_for_project(&d.conn(), pid)?))
        {
            Ok(rows) => rows
                .into_iter()
                .filter(|r| r.on_disk)
                .map(|r| (r.frame_uuid, r.content_version))
                .collect::<Vec<_>>(),
            Err(e) => {
                tracing::warn!(project_id = pid, error = %e, "collab maintenance: frame list failed; holder report skipped");
                continue;
            }
        };
        match report_holders(ctx, pid, &present).await {
            Ok(_) => outcome.reported += 1,
            Err(e) => {
                tracing::warn!(project_id = pid, error = %e, "collab maintenance: holder report failed")
            }
        }
        match recheck_awaiting_gc(ctx, pid).await {
            Ok(n) => outcome.cleared += n,
            Err(e) => {
                tracing::warn!(project_id = pid, error = %e, "collab maintenance: parked-frame recheck failed")
            }
        }
        if let Err(e) = sweep_in_flight(ctx, pid).await {
            tracing::warn!(project_id = pid, error = %e, "collab maintenance: in-flight sweep failed");
        }
    }
    tracing::info!(
        projects = outcome.projects,
        reported = outcome.reported,
        missing = outcome.missing,
        cleared = outcome.cleared,
        "collab maintenance complete"
    );
    outcome
}

/// The loss guard (P14): when this pass's missing replicas exceed
/// `collab.loss_guard_fraction` of the held ones, or `collab.loss_guard_bytes`
/// — and at least two are missing, so one deletion never trips it — pause the
/// project's replication and emit [`COLLAB_REPLICATION_PAUSED_EVENT`] once.
/// Returns whether it tripped. The caller skips it when the project is
/// already paused.
pub(crate) fn loss_guard(
    ctx: &ServiceContext,
    project_id: &str,
    truth: &DiskTruth,
    emitter: Option<&dyn ProgressEmitter>,
) -> Result<bool, ApiError> {
    let missing = truth.missing_replicas.len();
    if missing < 2 {
        return Ok(false);
    }
    let db = db(ctx)?;
    let conn = db.conn();
    let fraction = ctx
        .settings
        .get_collab_loss_guard_fraction(&conn)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %format!("{e:#}"), "loss guard fraction unreadable; default used");
            crate::settings::defaults::COLLAB_LOSS_GUARD_FRACTION
                .parse()
                .unwrap_or(0.10)
        });
    let bytes = ctx
        .settings
        .get_collab_loss_guard_bytes(&conn)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %format!("{e:#}"), "loss guard bytes unreadable; default used");
            crate::settings::defaults::COLLAB_LOSS_GUARD_BYTES
                .parse()
                .unwrap_or(i64::MAX)
        });
    let held = truth.held_replicas.max(missing);
    let tripped = (missing as f64 / held as f64) > fraction || truth.missing_bytes > bytes;
    if !tripped {
        return Ok(false);
    }
    crate::db::collab::set_replication_paused(&conn, project_id, true)?;
    drop(conn);
    tracing::warn!(
        project_id,
        count = missing,
        bytes = truth.missing_bytes,
        "replication paused by the loss guard"
    );
    if let Some(em) = emitter {
        crate::events::emit_event(
            em,
            COLLAB_REPLICATION_PAUSED_EVENT,
            &CollabReplicationPaused {
                project_id: project_id.to_string(),
                missing,
                missing_bytes: truth.missing_bytes,
            },
        );
    }
    Ok(true)
}

/// The FULL holder report (P8): `PUT …/holders/self` with exactly the frames
/// on disk. Above [`HOLDERS_CHUNK`] frames the first chunk goes `full` and
/// the rest as `add`. An empty set is still sent — "I hold nothing here" is
/// what clears a phantom holder (audit F5).
///
/// Signed out ⇒ `Ok(0)`. A 403 folds into `Ok(0)` at `debug!` (a membership
/// the hub will not take holds from); anything else is returned for the
/// caller to log and step over.
pub(crate) async fn report_holders(
    ctx: &ServiceContext,
    project_id: &str,
    present: &[(String, i32)],
) -> Result<usize, ApiError> {
    use crate::collab::hub_client::HolderRefWire;
    let Some((hub_url, token)) = crate::api::account::hub_credentials(ctx)? else {
        return Ok(0);
    };
    let client = CollabClient::new(&hub_url).map_err(client_err)?;
    let refs: Vec<HolderRefWire> = present
        .iter()
        .map(|(u, v)| HolderRefWire {
            frame_uuid: u.clone(),
            content_version: *v,
        })
        .collect();
    let chunks: Vec<&[HolderRefWire]> = if refs.is_empty() {
        vec![&refs[..]]
    } else {
        refs.chunks(HOLDERS_CHUNK).collect()
    };
    for (i, chunk) in chunks.into_iter().enumerate() {
        match client
            .put_holders(&token, project_id, i == 0, chunk, &[])
            .await
        {
            Ok(()) => {}
            Err(crate::account::AccountClientError::Forbidden) => {
                tracing::debug!(project_id, count = refs.len(), "holder report forbidden");
                return Ok(0);
            }
            Err(e) => {
                tracing::warn!(project_id, count = refs.len(), error = %e, "holder report failed");
                return Err(client_err(e));
            }
        }
    }
    tracing::info!(project_id, count = refs.len(), "holders reported");
    Ok(refs.len())
}

/// One replication fetch per project at a time, per catalog (ruling on the
/// audit's `PackagePullClaim`): a "Sync now" that lands while the worker is
/// fetching the same project returns at once — the running fetch IS the
/// requested work. Released on every exit, a panic included.
static IN_FLIGHT_FRAME_PULLS: std::sync::OnceLock<std::sync::Mutex<HashSet<String>>> =
    std::sync::OnceLock::new();

struct FramePullClaim(String);

impl FramePullClaim {
    fn acquire(key: &str) -> Option<Self> {
        let set = IN_FLIGHT_FRAME_PULLS.get_or_init(Default::default);
        match set.lock() {
            Ok(mut set) => set
                .insert(key.to_string())
                .then(|| FramePullClaim(key.to_string())),
            // Permissive on poison: refusing every fetch after one unrelated
            // panic would be the worse failure.
            Err(_) => Some(FramePullClaim(key.to_string())),
        }
    }
}

impl Drop for FramePullClaim {
    fn drop(&mut self) {
        if let Some(set) = IN_FLIGHT_FRAME_PULLS.get() {
            if let Ok(mut set) = set.lock() {
                set.remove(&self.0);
            }
        }
    }
}

/// `catalog|project` — the key of every per-project fetch registry.
fn fetch_key(ctx: &ServiceContext, project_id: &str) -> Result<String, ApiError> {
    Ok(format!("{}|{project_id}", db(ctx)?.path().display()))
}

/// The cancel flag of each running fetch (ruling R19), checked between
/// batches.
static FETCH_CANCELS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, Arc<std::sync::atomic::AtomicBool>>>,
> = std::sync::OnceLock::new();

/// A running fetch's registered cancel flag; unregistered on drop.
struct CancelRegistration(String, Arc<std::sync::atomic::AtomicBool>);

impl CancelRegistration {
    fn register(key: &str) -> Self {
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        if let Ok(mut map) = FETCH_CANCELS.get_or_init(Default::default).lock() {
            map.insert(key.to_string(), Arc::clone(&flag));
        }
        CancelRegistration(key.to_string(), flag)
    }
}

impl Drop for CancelRegistration {
    fn drop(&mut self) {
        if let Some(map) = FETCH_CANCELS.get() {
            if let Ok(mut map) = map.lock() {
                if map.get(&self.0).is_some_and(|f| Arc::ptr_eq(f, &self.1)) {
                    map.remove(&self.0);
                }
            }
        }
    }
}

/// Ask a running fetch of `project_id` to stop after its current batch
/// (R19). Returns whether one was running. Called when auto-replication is
/// turned off and when the project is lost; the between-batch re-check
/// covers both too, this only makes the stop explicit.
pub(crate) fn cancel_project_fetch(ctx: &ServiceContext, project_id: &str) -> bool {
    let Ok(key) = fetch_key(ctx, project_id) else {
        return false;
    };
    let flag = FETCH_CANCELS
        .get()
        .and_then(|m| m.lock().ok().and_then(|m| m.get(&key).cloned()));
    match flag {
        Some(flag) => {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            tracing::info!(project_id, "replication fetch cancel requested");
            true
        }
        None => false,
    }
}

/// Record a frame-level failure on its row (logged by the caller).
fn record_frame_error(ctx: &ServiceContext, project_id: &str, frame_uuid: &str, error: &str) {
    match db(ctx) {
        Ok(db) => {
            if let Err(e) =
                crate::db::collab_frames::set_error(&db.conn(), project_id, frame_uuid, Some(error))
            {
                tracing::warn!(project_id, frame_uuid, error = %format!("{e:#}"), "record frame error failed");
            }
        }
        Err(e) => tracing::warn!(project_id, frame_uuid, error = %e, "record frame error failed"),
    }
}

/// Everything one fetch needs, resolved once per [`fetch_frames`] call.
struct FetchEnv<'a> {
    ctx: &'a ServiceContext,
    node: Arc<crate::sharing::iroh::node::SharedIrohNode>,
    store: iroh_blobs::api::Store,
    client: CollabClient,
    token: String,
    relay_urls: Vec<String>,
    own: NodeId,
    /// The project row at fetch start (its id and slug name the landing
    /// folder); the live state is re-read between batches.
    project: crate::db::collab::CollabProjectRow,
    collab_root: PathBuf,
    started_at: String,
    /// "Sync now": the auto-replicate toggle does not stop this fetch.
    forced: bool,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    /// The project's disk lock (R18), held per frame while it lands.
    lock: Arc<tokio::sync::Mutex<()>>,
}

impl FetchEnv<'_> {
    fn pid(&self) -> &str {
        &self.project.project_id
    }

    fn in_flight_tag(&self, row: &LocalFrameRow) -> String {
        crate::sharing::iroh::blobs::in_flight_tag(&crate::sharing::iroh::node::project_frame_tag(
            self.pid(),
            &row.frame_uuid,
            row.content_version,
        ))
    }
}

/// Fetch and land `need` (P11, P21, P24). Resolves the receive gate and runs
/// [`fetch_frames_gated`].
pub(crate) async fn fetch_frames(
    ctx: &ServiceContext,
    sync: &crate::sync::SyncRuntime,
    project_id: &str,
    need: Vec<LocalFrameRow>,
    forced: bool,
    emitter: Option<&dyn ProgressEmitter>,
) -> Result<FetchOutcome, ApiError> {
    let control = sync.inbound_control().await;
    fetch_frames_gated(
        ctx,
        control.as_ref().map(|c| &c.receive_gate),
        project_id,
        need,
        forced,
        emitter,
    )
    .await
}

/// The fetch loop. Batches of up to [`FETCH_BATCH`] frames that HAVE fresh
/// holders run back to back (a frame no holder serves is skipped without
/// taking a slot, R16) until the queue is empty or a batch that attempted
/// fetches landed nothing. Each batch is ONE assignment run on the collab
/// ALPN under ONE `ReceiveGate` permit, released between batches so a
/// personal-sync receive gets its turn (R17). Before each batch the project
/// is re-read — lost, paused, auto-replicate turned off (unless `forced`),
/// cancelled — and the queue is re-filtered against the current rows and
/// policy (R19).
///
/// Before a frame is fetched its store entry is checked (P20): a `Dead`
/// entry is parked for GC (a fetch over it would panic iroh-blobs 0.103); a
/// `Readable` one whose content another frame of the project already landed
/// is linked from that file instead (P24). After each batch the landed
/// frames go to the hub as one holder `add` (P8).
pub(crate) async fn fetch_frames_gated(
    ctx: &ServiceContext,
    gate: Option<&crate::sync::ReceiveGate>,
    project_id: &str,
    need: Vec<LocalFrameRow>,
    forced: bool,
    emitter: Option<&dyn ProgressEmitter>,
) -> Result<FetchOutcome, ApiError> {
    let mut outcome = FetchOutcome::default();
    if need.is_empty() {
        return Ok(outcome);
    }
    let key = fetch_key(ctx, project_id)?;
    let Some(_claim) = FramePullClaim::acquire(&key) else {
        tracing::info!(
            project_id,
            "replication skipped: a fetch of this project is already running"
        );
        return Ok(outcome);
    };
    let cancel = CancelRegistration::register(&key);
    let collab_root = require_collaboration_root(ctx)?;
    if !collab_root.is_dir() {
        tracing::warn!(project_id, path = %collab_root.display(), "collaboration folder is not reachable; replication fetch skipped");
        return Ok(outcome);
    }
    let project = {
        let db = db(ctx)?;
        let conn = db.conn();
        live_project(&conn, project_id)?
    };
    let Some(node) = bound_node(ctx).await else {
        let e = ApiError::Internal("sync transport not started; cannot replicate".into());
        tracing::warn!(project_id, error = %e, "replication fetch refused");
        return Err(e);
    };
    let Some(store) = node.collab_store() else {
        let e = ApiError::Internal("the collaboration store is not mounted".into());
        tracing::warn!(project_id, error = %e, "replication fetch refused");
        return Err(e);
    };
    let Some((hub_url, token)) = crate::api::account::hub_credentials(ctx)? else {
        tracing::debug!(project_id, "replication fetch skipped: signed out");
        return Ok(outcome);
    };
    let client = CollabClient::new(&hub_url).map_err(client_err)?;
    // The node's own relay set (already resolved and applied at bind / on
    // refresh) completes each holder's hint; no hub round trip here.
    let relay_urls = node.relay_urls();
    let own = node.node_id();
    let env = FetchEnv {
        ctx,
        node,
        store,
        client,
        token,
        relay_urls,
        own,
        project,
        collab_root,
        started_at: crate::sync::now_iso(),
        forced,
        cancel: Arc::clone(&cancel.1),
        lock: project_disk_lock(ctx, project_id)?,
    };

    let total = need.len();
    tracing::info!(project_id, count = total, "replication fetch started");
    let mut queue: std::collections::VecDeque<LocalFrameRow> = need.into();
    let mut hinted: HashSet<NodeId> = HashSet::new();
    loop {
        if !between_batches(&env, &mut queue)? || queue.is_empty() {
            break;
        }
        let mut batch = prepare_batch(&env, &mut queue, &mut hinted, &mut outcome).await?;
        let attempted = batch.fetches.len() + batch.local.len();
        let fetched_landed = if attempted > 0 {
            // R17: one permit per batch, never across batches.
            let _permit = match gate {
                Some(gate) => Some(gate.acquire().await),
                None => None,
            };
            run_batch(&env, &mut batch, &mut outcome).await
        } else {
            0
        };
        if !batch.landed.is_empty() {
            // Folded after logging: the maintenance loop's full report
            // repairs a missed delta (P8).
            if let Err(e) = env
                .client
                .put_holders(&env.token, project_id, false, &batch.landed, &[])
                .await
            {
                tracing::warn!(project_id, count = batch.landed.len(), error = %e, "holder delta after landing failed");
            }
        }
        if batch.hub_failed {
            break;
        }
        if attempted > 0 && fetched_landed == 0 {
            if !queue.is_empty() {
                tracing::info!(
                    project_id,
                    count = queue.len(),
                    "replication stops early: a batch landed nothing"
                );
            }
            break;
        }
    }
    tracing::info!(
        project_id,
        count = total,
        landed = outcome.landed,
        failed = outcome.failed,
        awaiting_gc = outcome.awaiting_gc,
        "replication fetch finished"
    );
    if let Some(em) = emitter {
        if outcome.landed > 0 {
            crate::events::emit_event(
                em,
                COLLAB_FRAMES_LANDED_EVENT,
                &CollabFramesLanded {
                    project_id: project_id.to_string(),
                    landed: outcome.landed,
                    failed: outcome.failed,
                    awaiting_gc: outcome.awaiting_gc,
                },
            );
        }
    }
    Ok(outcome)
}

/// The re-check before every batch (R19). `false` = stop: cancelled, the
/// Collaboration folder unreachable, the project lost or paused, its role no
/// longer replicating, or auto-replicate turned off (unless forced).
/// Otherwise the queue keeps only frames still wanted at the queued version
/// and content under the current policy.
fn between_batches(
    env: &FetchEnv<'_>,
    queue: &mut std::collections::VecDeque<LocalFrameRow>,
) -> Result<bool, ApiError> {
    let pid = env.pid();
    if env.cancel.load(std::sync::atomic::Ordering::SeqCst) {
        tracing::info!(
            project_id = pid,
            count = queue.len(),
            "replication fetch cancelled"
        );
        return Ok(false);
    }
    if !env.collab_root.is_dir() {
        tracing::warn!(project_id = pid, path = %env.collab_root.display(), "collaboration folder is not reachable; replication fetch stops");
        return Ok(false);
    }
    let db = db(env.ctx)?;
    let conn = db.conn();
    let project = match live_project(&conn, pid) {
        Ok(p) => p,
        Err(ApiError::Invalid(_)) => {
            tracing::info!(
                project_id = pid,
                "project no longer joined; replication fetch stops"
            );
            return Ok(false);
        }
        Err(e) => {
            tracing::warn!(project_id = pid, error = %e, "catalog read failed; replication fetch stops this pass");
            return Ok(false);
        }
    };
    let stop = if project.replication_paused {
        Some("paused")
    } else if !role_allows_replication(&project.data_role, project.is_coordinator) {
        Some("role")
    } else if !env.forced && !project.auto_replicate {
        Some("auto_replicate_off")
    } else {
        None
    };
    if let Some(outcome) = stop {
        tracing::info!(project_id = pid, outcome, "replication fetch stops");
        return Ok(false);
    }
    let policy = read_policy(&project);
    let rows: std::collections::HashMap<String, LocalFrameRow> =
        crate::db::collab_frames::list_for_project(&conn, pid)?
            .into_iter()
            .map(|r| (r.frame_uuid.clone(), r))
            .collect();
    let before = queue.len();
    queue.retain(|q| {
        rows.get(&q.frame_uuid)
            .is_some_and(|r| still_wanted(r, q.content_version, &policy) && r.blake3 == q.blake3)
    });
    if queue.len() != before {
        tracing::debug!(
            project_id = pid,
            count = before - queue.len(),
            "frames no longer wanted dropped from the fetch"
        );
    }
    Ok(true)
}

/// A project, the live one, or `NotFound` for an unknown or lost project
/// (R14: a lost project is never acted on) — every ACTION path that takes a
/// project id reads through here (carried from the Task 8 review to wave 2
/// Task 11), with a message that tells the two refusals apart: an id this
/// device never cached still says "refresh first"; one it was removed from
/// says so distinctly, via [`crate::db::collab::get_live_project`].
pub(crate) fn live_project(
    conn: &rusqlite::Connection,
    project_id: &str,
) -> Result<crate::db::collab::CollabProjectRow, ApiError> {
    if crate::db::collab::get_project(conn, project_id)?.is_none() {
        return Err(ApiError::NotFound(format!(
            "project {project_id} is not cached — refresh first"
        )));
    }
    crate::db::collab::get_live_project(conn, project_id)?
        .ok_or_else(|| ApiError::NotFound("project no longer joined".into()))
}

/// A landed, on-disk frame of the same project with this content (P24).
fn identical_landed(
    ctx: &ServiceContext,
    row: &LocalFrameRow,
) -> Result<Option<PathBuf>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    Ok(
        crate::db::collab_frames::find_by_project_and_xxh3(&conn, &row.project_id, &row.xxh3)?
            .into_iter()
            .filter(|r| r.frame_uuid != row.frame_uuid && r.on_disk && r.blake3 == row.blake3)
            .filter_map(|r| r.landed_path.map(PathBuf::from))
            .find(|p| p.is_file()),
    )
}

/// One batch, prepared: what to fetch, what to land straight from the store,
/// and who follows whom (identical content, fetched once).
#[derive(Default)]
struct Batch {
    fetches: Vec<crate::sharing::iroh::blobs::FrameFetch>,
    rows: std::collections::HashMap<String, (LocalFrameRow, iroh_blobs::Hash)>,
    followers: std::collections::HashMap<String, Vec<LocalFrameRow>>,
    local: Vec<(LocalFrameRow, iroh_blobs::Hash)>,
    /// Holder entries for everything landed in this batch (links included).
    landed: Vec<crate::collab::hub_client::HolderRefWire>,
    /// A holder lookup failed hub-wide (R25): this batch is the fetch's last.
    hub_failed: bool,
}

/// Is a holder-lookup failure about THIS frame only — the hub answers 404
/// for a frame it no longer shows this device? Everything else (transport,
/// 5xx, 429, 401/403, …) is the hub failing and stops the fetch (R25).
fn holder_lookup_is_frame_level(e: &crate::account::AccountClientError) -> bool {
    matches!(e, crate::account::AccountClientError::Network(m)
        if m.starts_with("hub returned 404"))
}

/// Fill one batch from the queue (R16): pop frames until [`FETCH_BATCH`] of
/// them are to be fetched or landed from the store, or the queue is empty.
/// A frame with no fresh holder is skipped (`debug!`) and takes no slot; a
/// dead entry is parked (P20); identical content already on disk is linked
/// right here (P24).
async fn prepare_batch(
    env: &FetchEnv<'_>,
    queue: &mut std::collections::VecDeque<LocalFrameRow>,
    hinted: &mut HashSet<NodeId>,
    outcome: &mut FetchOutcome,
) -> Result<Batch, ApiError> {
    use crate::sharing::iroh::blobs::FrameFetch;
    use crate::sharing::iroh::node::BlobHealth;

    let pid = env.pid().to_string();
    let pid = pid.as_str();
    let mut batch = Batch::default();
    let mut rep_of_hash: std::collections::HashMap<iroh_blobs::Hash, String> = Default::default();
    while batch.fetches.len() + batch.local.len() < FETCH_BATCH {
        let Some(row) = queue.pop_front() else { break };
        let uuid = row.frame_uuid.clone();
        let hash = match row.blake3.parse::<iroh_blobs::Hash>() {
            Ok(h) => h,
            Err(e) => {
                let msg = format!("frame blake3 does not parse: {e}");
                tracing::warn!(project_id = pid, frame_uuid = %uuid, error = %msg, "frame skipped");
                record_frame_error(env.ctx, pid, &uuid, &msg);
                outcome.failed += 1;
                continue;
            }
        };
        if let Some(rep) = rep_of_hash.get(&hash) {
            batch.followers.entry(rep.clone()).or_default().push(row);
            continue;
        }
        let health = match env.node.collab_blob_health(hash).await {
            Ok(h) => h,
            Err(e) => {
                record_frame_error(env.ctx, pid, &uuid, &format!("{e:#}"));
                outcome.failed += 1;
                continue;
            }
        };
        match health {
            BlobHealth::Dead => {
                // P20: never fetch over a dead entry. Park until GC.
                tracing::warn!(project_id = pid, frame_uuid = %uuid, "frame content is a dead store entry; waiting for GC");
                let _guard = env.lock.lock().await;
                if fresh_row(env, &row)?.is_none() {
                    continue;
                }
                unseed_frame(Some(&env.node), pid, &uuid).await;
                let db = db(env.ctx)?;
                crate::db::collab_frames::set_missing(&db.conn(), pid, &uuid, true)?;
                outcome.awaiting_gc += 1;
                continue;
            }
            BlobHealth::Readable => {
                if let Some(src) = identical_landed(env.ctx, &row)? {
                    match link_identical(env, &row, &src).await {
                        Landed::Yes(_) => {
                            batch.landed.push(holder_ref(&row));
                            outcome.landed += 1;
                        }
                        Landed::Failed => outcome.failed += 1,
                        Landed::AwaitingGc | Landed::Stale => {}
                    }
                    continue;
                }
                // Complete in the store already (a fetch that died before
                // its landing): land it straight from the store.
                rep_of_hash.insert(hash, uuid.clone());
                batch.local.push((row, hash));
                continue;
            }
            BlobHealth::Missing | BlobHealth::Partial => {}
        }
        let holders = match env.client.frame_holders(&env.token, pid, &uuid).await {
            Ok(h) => h,
            Err(crate::account::AccountClientError::CollabApiOutdated) => {
                return Err(client_err(
                    crate::account::AccountClientError::CollabApiOutdated,
                ));
            }
            Err(e) if holder_lookup_is_frame_level(&e) => {
                // The hub no longer shows this one frame (hidden, rejected):
                // skip it alone.
                let msg = format!("holder lookup failed: {e}");
                tracing::warn!(project_id = pid, frame_uuid = %uuid, error = %e, "frame holder lookup failed");
                record_frame_error(env.ctx, pid, &uuid, &msg);
                outcome.failed += 1;
                continue;
            }
            Err(e) => {
                // R25: the hub itself is failing (transport, 5xx, 429, auth).
                // Asking it again for every other frame would cost one
                // request, one warn and one row error per frame per pass:
                // stop preparing, fetch what is ready, leave the rest.
                let msg = format!("holder lookup failed: {e}");
                tracing::warn!(project_id = pid, frame_uuid = %uuid, count = queue.len(), error = %e, "hub holder lookups failing; the rest of the need set waits for the next pass");
                record_frame_error(env.ctx, pid, &uuid, &msg);
                outcome.failed += 1;
                batch.hub_failed = true;
                break;
            }
        };
        let providers: Vec<(NodeId, Option<String>)> = holders
            .iter()
            .filter_map(|h| {
                pairing::node_id_from_pubkey_b64(&h.pubkey)
                    .ok()
                    .map(|n| (n, h.relay_url.clone()))
            })
            .filter(|(n, _)| *n != env.own)
            .collect();
        if providers.is_empty() {
            tracing::debug!(project_id = pid, frame_uuid = %uuid, "no fresh holder");
            continue;
        }
        // Dial hints, once per provider per fetch. Cross-account: relay only
        // (S1). A hint that cannot be built costs that provider its dial, not
        // the frame — the others may still serve.
        for (holder, relay) in &providers {
            if !hinted.insert(*holder) {
                continue;
            }
            let reported = relay
                .as_ref()
                .map(|url| crate::account::EndpointAddrReport {
                    home_relay_url: Some(url.clone()),
                    direct_addrs: Vec::new(),
                    reported_at: None,
                });
            match pairing::peer_dial_addr(*holder, reported.as_ref(), &env.relay_urls, true) {
                Ok(addr) => env.node.add_peer(addr),
                Err(e) => tracing::warn!(
                    error = %format!("{e:#}"),
                    holder = %node_id_hex(holder),
                    "replication: dial hint build failed for one provider"
                ),
            }
        }
        batch.fetches.push(FrameFetch {
            key: uuid.clone(),
            hash,
            size: row.byte_size.max(0) as u64,
            providers: providers
                .iter()
                .filter_map(|(n, _)| iroh::EndpointId::from_bytes(n).ok())
                .collect(),
            in_flight_tag: env.in_flight_tag(&row),
        });
        rep_of_hash.insert(hash, uuid.clone());
        batch.rows.insert(uuid, (row, hash));
    }
    Ok(batch)
}

fn holder_ref(row: &LocalFrameRow) -> crate::collab::hub_client::HolderRefWire {
    crate::collab::hub_client::HolderRefWire {
        frame_uuid: row.frame_uuid.clone(),
        content_version: row.content_version,
    }
}

/// Run one prepared batch: fetch, then land every fetched (or store-local)
/// frame and link its followers. Returns how many frames landed. A transfer
/// failure keeps the frame's in-flight tag — the next fetch resumes from
/// its verified partial bytes (P22, R23) — and a whole-batch failure still
/// lands what is already complete (m1).
async fn run_batch(env: &FetchEnv<'_>, batch: &mut Batch, outcome: &mut FetchOutcome) -> usize {
    let pid = env.pid().to_string();
    let pid = pid.as_str();
    let mut landed = 0;
    let mut to_land: Vec<(LocalFrameRow, iroh_blobs::Hash)> = Vec::new();
    for (row, hash) in std::mem::take(&mut batch.local) {
        let tag = env.in_flight_tag(&row);
        if let Err(e) = env
            .store
            .tags()
            .set(&tag, iroh_blobs::HashAndFormat::raw(hash))
            .await
        {
            let msg = format!("set in-flight tag {tag}: {e}");
            tracing::warn!(project_id = pid, frame_uuid = %row.frame_uuid, error = %msg, "frame skipped");
            record_frame_error(env.ctx, pid, &row.frame_uuid, &msg);
            outcome.failed += 1;
            continue;
        }
        to_land.push((row, hash));
    }

    let fetches = std::mem::take(&mut batch.fetches);
    if !fetches.is_empty() {
        let telemetry: ProviderTelemetrySink = {
            let pid = pid.to_string();
            Arc::new(move |ev| match ev {
                ProviderEvent::Trying(id) => {
                    tracing::debug!(project_id = %pid, holder = %node_id_hex(&id), "replication: provider tried")
                }
                ProviderEvent::Failed(id) => {
                    tracing::debug!(project_id = %pid, holder = %node_id_hex(&id), "replication: provider failed; switching")
                }
            })
        };
        match crate::sharing::iroh::blobs::fetch_blobs_assigned(
            &env.store,
            &env.node.endpoint(),
            fetches,
            telemetry,
        )
        .await
        {
            Ok(results) => {
                for (uuid, r) in results {
                    let Some((row, hash)) = batch.rows.remove(&uuid) else {
                        continue;
                    };
                    match r {
                        Ok(()) => to_land.push((row, hash)),
                        Err(e) => {
                            let msg = format!("{e:#}");
                            tracing::warn!(project_id = pid, frame_uuid = %uuid, error = %msg, "frame fetch failed; its partial bytes stay for the next attempt");
                            fail_with_followers(env, batch, &uuid, &msg, outcome);
                        }
                    }
                }
            }
            Err(e) => {
                let msg = format!("{e:#}");
                tracing::error!(project_id = pid, error = %msg, "replication batch fetch failed");
                let uuids: Vec<String> = batch.rows.keys().cloned().collect();
                for uuid in uuids {
                    batch.rows.remove(&uuid);
                    fail_with_followers(env, batch, &uuid, &msg, outcome);
                }
            }
        }
    }

    for (row, hash) in to_land {
        let uuid = row.frame_uuid.clone();
        let followers = batch.followers.remove(&uuid).unwrap_or_default();
        match land_frame(env, &row, hash).await {
            Landed::Yes(dest) => {
                batch.landed.push(holder_ref(&row));
                outcome.landed += 1;
                landed += 1;
                for f in followers {
                    match link_identical(env, &f, &dest).await {
                        Landed::Yes(_) => {
                            batch.landed.push(holder_ref(&f));
                            outcome.landed += 1;
                            landed += 1;
                        }
                        Landed::Failed => outcome.failed += 1,
                        Landed::AwaitingGc | Landed::Stale => {}
                    }
                }
            }
            Landed::AwaitingGc => {
                outcome.awaiting_gc += 1;
            }
            Landed::Stale => {}
            Landed::Failed => {
                outcome.failed += 1 + followers.len();
                for f in followers {
                    record_frame_error(
                        env.ctx,
                        pid,
                        &f.frame_uuid,
                        "identical frame failed to land",
                    );
                }
            }
        }
    }
    landed
}

/// Record a transfer failure on a frame and on every frame that was to be
/// linked from it.
fn fail_with_followers(
    env: &FetchEnv<'_>,
    batch: &mut Batch,
    uuid: &str,
    msg: &str,
    outcome: &mut FetchOutcome,
) {
    let pid = env.pid();
    record_frame_error(env.ctx, pid, uuid, msg);
    outcome.failed += 1;
    for f in batch.followers.remove(uuid).unwrap_or_default() {
        record_frame_error(env.ctx, pid, &f.frame_uuid, msg);
        outcome.failed += 1;
    }
}

/// Delete one tag, logging a failure (the store's open sweep and the
/// maintenance sweep reclaim a stale in-flight tag).
async fn drop_tag(store: &iroh_blobs::api::Store, tag: &str) {
    if let Err(e) = store.tags().delete(tag).await {
        tracing::warn!(tag, error = %e, "delete tag failed");
    }
}

/// What landing (or linking) one frame did.
enum Landed {
    Yes(PathBuf),
    /// The store's data went away under us (export found no source): the
    /// tags are dropped and the frame waits for GC (P20). Not a failure.
    AwaitingGc,
    /// The row moved on (a new version, other content) while the bytes were
    /// in flight (R20): nothing recorded, left for the next pass.
    Stale,
    /// Logged and recorded on the row.
    Failed,
}

/// The row as it is NOW, or `None` (logged) when it no longer describes the
/// bytes in hand — a manifest sync moved its version or content, it went
/// away (R20), or it is already on disk (re-admitted meanwhile, N3).
fn fresh_row(env: &FetchEnv<'_>, row: &LocalFrameRow) -> Result<Option<LocalFrameRow>, ApiError> {
    let db = db(env.ctx)?;
    let fresh = crate::db::collab_frames::get(&db.conn(), env.pid(), &row.frame_uuid)?;
    match fresh {
        Some(f)
            if f.content_version == row.content_version && f.blake3 == row.blake3 && !f.on_disk =>
        {
            Ok(Some(f))
        }
        _ => {
            tracing::info!(
                project_id = env.pid(),
                frame_uuid = %row.frame_uuid,
                content_version = row.content_version,
                "frame changed while in flight; left for the next pass"
            );
            Ok(None)
        }
    }
}

/// Where a NEW landing goes (P10, P21): `unique_path(<Collab>/<project>/
/// <publisher>/<fileName>)`, the file name checked first.
fn new_landing_path(env: &FetchEnv<'_>, row: &LocalFrameRow) -> Result<PathBuf, String> {
    crate::package::validate_rel_path(&row.file_name)
        .map_err(|e| format!("unsafe file name {:?}: {e:#}", row.file_name))?;
    let dir = db(env.ctx)
        .map_err(anyhow::Error::from)
        .and_then(|db| {
            publisher_folder(
                &db.conn(),
                &env.collab_root,
                &env.project,
                &row.publisher_account_id,
                &row.publisher_display,
                "publisher",
            )
        })
        .map_err(|e| format!("publisher folder: {e:#}"))?;
    Ok(crate::sync::ingest::unique_path(&dir.join(
        crate::sync::ingest::native_rel_path(&row.file_name),
    )))
}

/// The target of a landing (fetched or linked). A row that already has a
/// path inside the Collaboration root lands there, its old tags dropped
/// first:
///
/// - a NEW VERSION (a version bump clears `size_mtime_seen`) lands OVER the
///   old file under the same name (plan step 7.5, owner-approved);
/// - a SAME-VERSION re-land (the file went missing or was edited) never
///   destroys what is there: a file at the path is renamed aside to
///   `unique_path` first — it becomes an inert foreign file (R24, R18).
///
/// Anything else goes to [`new_landing_path`].
async fn landing_target(env: &FetchEnv<'_>, row: &LocalFrameRow) -> Result<PathBuf, String> {
    let existing = row
        .landed_path
        .as_deref()
        .map(PathBuf::from)
        .filter(|p| inside_root(Some(&env.collab_root), row, p));
    let Some(dest) = existing else {
        return new_landing_path(env, row);
    };
    unseed_frame(Some(&env.node), env.pid(), &row.frame_uuid).await;
    if row.size_mtime_seen.is_some() && dest.exists() && !holds_frame_content(&dest, row).await {
        let aside = crate::sync::ingest::unique_path(&dest);
        std::fs::rename(&dest, &aside).map_err(|e| {
            format!(
                "keep the edited file {} aside as {}: {e}",
                dest.display(),
                aside.display()
            )
        })?;
        tracing::info!(
            project_id = env.pid(),
            frame_uuid = %row.frame_uuid,
            path = %dest.display(),
            dest = %aside.display(),
            "edited replica kept beside the re-fetched frame"
        );
    }
    Ok(dest)
}

/// Does the file at `path` already hold this frame's content (size first,
/// then xxh3)? Then a re-land simply goes over it — a byte-identical copy is
/// no edit worth keeping aside (N3).
async fn holds_frame_content(path: &Path, row: &LocalFrameRow) -> bool {
    match tokio::fs::metadata(path).await {
        Ok(m) if m.is_file() && m.len() as i64 == row.byte_size => {}
        _ => return false,
    }
    matches!(xxh3_on_blocking(path).await, Ok(h) if h == row.xxh3)
}

/// Land one fetched frame (P21), under the project's disk lock: re-read the
/// row (R20), pick the target, `export_child` straight to it — a rename of
/// the store's data file, so the landed file IS the seed — then the
/// permanent seed tag, then the in-flight tag goes, then one DB transaction
/// (row + `sync_history`) that only lands on the same version and content.
/// A stale landing drops its tags and removes nothing the row references.
async fn land_frame(env: &FetchEnv<'_>, row: &LocalFrameRow, hash: iroh_blobs::Hash) -> Landed {
    use crate::sharing::iroh::blobs;
    use crate::sharing::iroh::node::project_frame_tag;

    let pid = env.pid();
    let uuid = row.frame_uuid.as_str();
    let in_flight = env.in_flight_tag(row);
    let fail = |msg: String| {
        tracing::error!(project_id = pid, frame_uuid = uuid, error = %msg, "frame landing failed");
        record_frame_error(env.ctx, pid, uuid, &msg);
    };
    let _guard = env.lock.lock().await;
    let row = match fresh_row(env, row) {
        Ok(Some(r)) => r,
        Ok(None) => {
            drop_tag(&env.store, &in_flight).await;
            return Landed::Stale;
        }
        Err(e) => {
            fail(format!("re-read the frame: {e}"));
            return Landed::Failed;
        }
    };
    let dest = match landing_target(env, &row).await {
        Ok(d) => d,
        Err(msg) => {
            fail(msg);
            drop_tag(&env.store, &in_flight).await;
            return Landed::Failed;
        }
    };
    if let Some(parent) = dest.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            fail(format!("create {}: {e}", parent.display()));
            drop_tag(&env.store, &in_flight).await;
            return Landed::Failed;
        }
    }
    match blobs::export_child(&env.store, hash, &dest).await {
        Err(e) => {
            fail(format!("export to {}: {e:#}", dest.display()));
            drop_tag(&env.store, &in_flight).await;
            return Landed::Failed;
        }
        Ok(Err(e)) if blobs::export_source_vanished(&e) => {
            tracing::warn!(project_id = pid, frame_uuid = uuid, path = %dest.display(), error = %e, "frame data vanished before landing; waiting for GC");
            drop_tag(&env.store, &in_flight).await;
            unseed_frame(Some(&env.node), pid, uuid).await;
            if let Ok(db) = db(env.ctx) {
                if let Err(e) = crate::db::collab_frames::set_missing(&db.conn(), pid, uuid, true) {
                    tracing::warn!(project_id = pid, frame_uuid = uuid, error = %format!("{e:#}"), "mark frame awaiting GC failed");
                }
            }
            return Landed::AwaitingGc;
        }
        Ok(Err(e)) => {
            fail(format!("export to {}: {e}", dest.display()));
            drop_tag(&env.store, &in_flight).await;
            return Landed::Failed;
        }
        Ok(Ok(())) => {}
    }
    let tag = project_frame_tag(pid, uuid, row.content_version);
    if let Err(e) = env
        .store
        .tags()
        .set(&tag, iroh_blobs::HashAndFormat::raw(hash))
        .await
    {
        fail(format!("seed tag {tag}: {e}"));
        remove_landed(&dest);
        drop_tag(&env.store, &in_flight).await;
        return Landed::Failed;
    }
    drop_tag(&env.store, &in_flight).await;
    match record_landing(env, &row, &dest) {
        Ok(true) => {
            tracing::info!(project_id = pid, frame_uuid = uuid, path = %dest.display(), "frame landed");
            Landed::Yes(dest)
        }
        Ok(false) => {
            forget_stale_landing(env, &row, &tag, &dest).await;
            Landed::Stale
        }
        Err(e) => {
            fail(format!("record landing: {e:#}"));
            remove_landed(&dest);
            unseed_frame(Some(&env.node), pid, uuid).await;
            Landed::Failed
        }
    }
}

/// A landing the row moved away from while it was written (R20): drop the
/// seed tag it set, and remove the file only when the row does not reference
/// that path.
async fn forget_stale_landing(env: &FetchEnv<'_>, row: &LocalFrameRow, tag: &str, dest: &Path) {
    tracing::info!(
        project_id = env.pid(),
        frame_uuid = %row.frame_uuid,
        path = %dest.display(),
        "frame changed while landing; left for the next pass"
    );
    drop_tag(&env.store, tag).await;
    let referenced = db(env.ctx)
        .ok()
        .and_then(|db| {
            crate::db::collab_frames::get(&db.conn(), env.pid(), &row.frame_uuid)
                .ok()
                .flatten()
        })
        .and_then(|r| r.landed_path)
        .is_some_and(|p| Path::new(&p) == dest);
    if !referenced {
        remove_landed(dest);
    }
}

/// Remove a landed file after a failed step, logging a failure.
fn remove_landed(path: &Path) {
    if let Err(e) = std::fs::remove_file(path) {
        tracing::warn!(path = %path.display(), error = %e, "remove orphaned landed frame failed");
    }
}

/// One transaction: the row is landed — only while it still describes these
/// bytes (R20) — and a `sync_history` row records the receive, the way the
/// package ingest wrote it. `Ok(false)` = stale, nothing written.
fn record_landing(env: &FetchEnv<'_>, row: &LocalFrameRow, dest: &Path) -> Result<bool> {
    let meta = std::fs::metadata(dest).with_context(|| format!("stat {}", dest.display()))?;
    let sm = size_mtime_from(&meta);
    let db = db(env.ctx).map_err(|e| anyhow!("{e}"))?;
    let conn = db.conn();
    let tx = conn.unchecked_transaction().context("begin landing tx")?;
    let n = crate::db::collab_frames::set_landed_if(
        &tx,
        &row.project_id,
        &row.frame_uuid,
        &dest.to_string_lossy(),
        &sm,
        row.content_version,
        &row.blake3,
    )
    .context("mark frame landed")?;
    if n == 0 {
        return Ok(false);
    }
    crate::sync::store::insert_history_row(
        &tx,
        &crate::sync::HistoryRow {
            frame_uuid: row.frame_uuid.clone(),
            filename: row.file_name.clone(),
            object: None,
            // A swarm fetch has no single serving device.
            peer_device: "swarm".to_string(),
            direction: crate::sync::Direction::Received,
            bytes: row.byte_size.max(0) as u64,
            started_at: env.started_at.clone(),
            finished_at: Some(crate::sync::now_iso()),
            outcome: "ingested".to_string(),
            project: Some(row.project_id.clone()),
            package_id: None,
            batch_name: None,
        },
    )
    .context("insert sync_history row")?;
    tx.commit().context("commit landing tx")?;
    Ok(true)
}

/// P24: land a frame whose content another frame of the project already has
/// on disk — link (or copy) that file, seed it by reference, record it. No
/// fetch. Under the project's disk lock, on the row as it is now (R20).
async fn link_identical(env: &FetchEnv<'_>, row: &LocalFrameRow, src: &Path) -> Landed {
    let pid = env.pid();
    let uuid = row.frame_uuid.as_str();
    let fail = |msg: String| {
        tracing::warn!(project_id = pid, frame_uuid = uuid, error = %msg, "identical frame landing failed");
        record_frame_error(env.ctx, pid, uuid, &msg);
    };
    let _guard = env.lock.lock().await;
    let row = match fresh_row(env, row) {
        Ok(Some(r)) => r,
        Ok(None) => return Landed::Stale,
        Err(e) => {
            fail(format!("re-read the frame: {e}"));
            return Landed::Failed;
        }
    };
    tracing::warn!(project_id = pid, frame_uuid = uuid, path = %src.display(), "identical frame content in project");
    let dest = match landing_target(env, &row).await {
        Ok(d) => d,
        Err(msg) => {
            fail(msg);
            return Landed::Failed;
        }
    };
    // A new version lands over the old file: clear it for the link.
    if dest != src {
        if let Err(e) = std::fs::remove_file(&dest) {
            if e.kind() != std::io::ErrorKind::NotFound {
                fail(format!("remove stale {}: {e}", dest.display()));
                return Landed::Failed;
            }
        }
    }
    if let Some(parent) = dest.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            fail(format!("create {}: {e}", parent.display()));
            return Landed::Failed;
        }
    }
    if let Err(e) = crate::sync::ingest::link_or_copy(src, &dest, false) {
        fail(format!(
            "link {} -> {}: {e:#}",
            src.display(),
            dest.display()
        ));
        return Landed::Failed;
    }
    if let Err(e) = env
        .node
        .seed_project_frame(pid, uuid, row.content_version, &dest)
        .await
    {
        fail(format!("seed {}: {e:#}", dest.display()));
        remove_landed(&dest);
        return Landed::Failed;
    }
    match record_landing(env, &row, &dest) {
        Ok(true) => {
            tracing::info!(project_id = pid, frame_uuid = uuid, path = %dest.display(), "frame landed");
            Landed::Yes(dest)
        }
        Ok(false) => {
            let tag = crate::sharing::iroh::node::project_frame_tag(pid, uuid, row.content_version);
            forget_stale_landing(env, &row, &tag, &dest).await;
            Landed::Stale
        }
        Err(e) => {
            fail(format!("record landing: {e:#}"));
            remove_landed(&dest);
            unseed_frame(Some(&env.node), pid, uuid).await;
            Landed::Failed
        }
    }
}

// ── Replication policy + loss commands ───────────────────────────────────────

/// The stored policy of a live project. An unreadable document is logged and
/// read as the default (replicate everything) — the same as never set.
fn read_policy(project: &crate::db::collab::CollabProjectRow) -> ReplicationPolicy {
    serde_json::from_str(&project.policy_json).unwrap_or_else(|e| {
        tracing::warn!(project_id = %project.project_id, error = %e, "replication policy unreadable; replicating everything");
        ReplicationPolicy::default()
    })
}

/// What `policy` selects in the project's cached frames.
fn policy_preview(rows: &[LocalFrameRow], policy: &ReplicationPolicy) -> PolicyPreview {
    let matching: Vec<&LocalFrameRow> = rows
        .iter()
        .filter(|r| replicable(r) && policy_matches(r, policy))
        .collect();
    let to_fetch = frame_need(rows, policy, true, true, false);
    PolicyPreview {
        frames: matching.len(),
        bytes: matching.iter().map(|r| r.byte_size).sum(),
        already_held: matching.iter().filter(|r| r.on_disk).count(),
        to_fetch: to_fetch.len(),
        to_fetch_bytes: to_fetch.iter().map(|r| r.byte_size).sum(),
    }
}

fn validate_policy(policy: &ReplicationPolicy) -> Result<(), ApiError> {
    if let Some(f) = policy.max_fwhm_arcsec {
        if !f.is_finite() || f <= 0.0 {
            return Err(ApiError::Invalid(
                "the FWHM limit must be a positive number of arcseconds".into(),
            ));
        }
    }
    if policy.min_stars.is_some_and(|s| s < 0) {
        return Err(ApiError::Invalid(
            "the star count cannot be negative".into(),
        ));
    }
    if policy.byte_budget.is_some_and(|b| b < 0) {
        return Err(ApiError::Invalid(
            "the byte budget cannot be negative".into(),
        ));
    }
    Ok(())
}

/// The project's replication policy (local only).
pub async fn get_collab_policy(
    ctx: &ServiceContext,
    project_id: &str,
) -> Result<ReplicationPolicy, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let project = live_project(&conn, project_id)?;
    Ok(read_policy(&project))
}

/// What `policy` would select, without storing it.
pub async fn preview_collab_policy(
    ctx: &ServiceContext,
    project_id: &str,
    policy: ReplicationPolicy,
) -> Result<PolicyPreview, ApiError> {
    validate_policy(&policy)?;
    let db = db(ctx)?;
    let conn = db.conn();
    live_project(&conn, project_id)?;
    let rows = crate::db::collab_frames::list_for_project(&conn, project_id)?;
    Ok(policy_preview(&rows, &policy))
}

/// Store the project's replication policy and return what it selects. The
/// worker is kicked, so a widened policy starts fetching without waiting for
/// the cadence. A narrowed one never deletes anything already held.
pub async fn set_collab_policy(
    ctx: &ServiceContext,
    project_id: &str,
    policy: ReplicationPolicy,
) -> Result<PolicyPreview, ApiError> {
    validate_policy(&policy)?;
    let preview = {
        let db = db(ctx)?;
        let conn = db.conn();
        live_project(&conn, project_id)?;
        let json = serde_json::to_string(&policy)
            .map_err(|e| ApiError::Internal(format!("encode replication policy: {e}")))?;
        crate::db::collab::set_policy(&conn, project_id, &json)?;
        let rows = crate::db::collab_frames::list_for_project(&conn, project_id)?;
        policy_preview(&rows, &policy)
    };
    tracing::info!(
        project_id,
        count = preview.to_fetch,
        "replication policy set"
    );
    auto_sync_kick().notify_one();
    Ok(preview)
}

/// Adapts an optional shared emitter to the scanner's `&E` parameter.
struct OptEmitter(Option<Arc<dyn ProgressEmitter>>);

impl ProgressEmitter for OptEmitter {
    fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
        if let Some(em) = &self.0 {
            em.emit_json(event_name, payload);
        }
    }
}

/// Answer a paused project (P14).
///
/// - `Restore`: rescan the Collaboration root with the same entry the
///   Rescan button uses (the scanner repairs moved files), run disk truth
///   again so the found files are re-admitted, unpause and kick the worker.
/// - `StopHolding`: decline every replica whose landed file is really gone
///   or edited (never one waiting for a new version, R22), then unpause.
///   Nothing is deleted.
///
/// `_sync` keeps the command surface uniform with the other replication
/// commands; the resumed pass runs on the worker, kicked.
pub async fn resolve_collab_loss(
    ctx: Arc<ServiceContext>,
    _sync: Arc<crate::sync::SyncRuntime>,
    project_id: &str,
    action: LossAction,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Result<(), ApiError> {
    use rusqlite::OptionalExtension;
    {
        let db = db(&ctx)?;
        live_project(&db.conn(), project_id)?;
    }
    match action {
        LossAction::Restore => {
            let root_id: i64 = {
                let db = db(&ctx)?;
                let conn = db.conn();
                conn.query_row(
                    "SELECT id FROM scan_roots WHERE kind = 'collaboration' LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .optional()?
                .ok_or_else(|| ApiError::Invalid(COLLABORATION_ROOT_REQUIRED.to_string()))?
            };
            let scan_ctx = Arc::clone(&ctx);
            let em = OptEmitter(emitter.clone());
            tokio::task::spawn_blocking(move || {
                crate::api::scan_roots::start_scan_with_progress(&scan_ctx, root_id, &em)
            })
            .await
            .map_err(|e| ApiError::Internal(format!("restore scan task: {e}")))?
            .inspect_err(|e| {
                tracing::error!(project_id, error = %e, "restore rescan failed");
            })?;
            let truth = disk_truth(&ctx, project_id).await?;
            {
                let db = db(&ctx)?;
                crate::db::collab::set_replication_paused(&db.conn(), project_id, false)?;
            }
            tracing::info!(
                project_id,
                count = truth.present.len(),
                missing = truth.missing_replicas.len(),
                "replication resumed after restore"
            );
            auto_sync_kick().notify_one();
        }
        LossAction::StopHolding => {
            // R22: decline only what is really lost — a landed replica whose
            // file is gone or no longer its content. A row waiting for a new
            // version (a bump clears `size_mtime_seen`) is not a loss, and an
            // intact file is left for re-admission.
            let candidates: Vec<LocalFrameRow> = {
                let db = db(&ctx)?;
                crate::db::collab_frames::list_for_project(&db.conn(), project_id)?
                    .into_iter()
                    .filter(|r| {
                        r.origin == FrameOrigin::Replica
                            && !r.on_disk
                            && !r.locally_declined
                            && r.landed_path.is_some()
                            && r.size_mtime_seen.is_some()
                    })
                    .collect()
            };
            let mut missing: Vec<String> = Vec::new();
            for row in candidates {
                if replica_file_lost(&row).await {
                    missing.push(row.frame_uuid);
                }
            }
            let db = db(&ctx)?;
            let conn = db.conn();
            crate::db::collab_frames::set_declined(&conn, project_id, &missing, true)?;
            crate::db::collab::set_replication_paused(&conn, project_id, false)?;
            tracing::info!(
                project_id,
                count = missing.len(),
                "missing frames no longer held; replication resumed"
            );
        }
    }
    Ok(())
}

/// Is a landed replica's file really lost — gone, or no longer its content
/// (R22)? An unchanged `size:mtime` or a matching xxh3 says it is intact.
async fn replica_file_lost(row: &LocalFrameRow) -> bool {
    let Some(landed) = row.landed_path.as_deref() else {
        return true;
    };
    let path = Path::new(landed);
    let meta = match tokio::fs::metadata(path).await {
        Ok(m) if m.is_file() => m,
        _ => return true,
    };
    if row.size_mtime_seen.as_deref() == Some(size_mtime_from(&meta).as_str()) {
        return false;
    }
    match xxh3_on_blocking(path).await {
        Ok(h) => h != row.xxh3,
        Err(e) => {
            tracing::warn!(frame_uuid = %row.frame_uuid, path = %landed, error = %format!("{e:#}"), "hash of a lost frame failed; counted lost");
            true
        }
    }
}

/// How often the auto-replication worker sweeps every auto-enabled project
/// (spec §3.3). Long by design: a whole-swarm failure means every holder is
/// gone, and 20 minutes is an honest retry interval for that.
pub const COLLAB_AUTO_SYNC_INTERVAL: std::time::Duration = std::time::Duration::from_secs(20 * 60);

/// Grace before the FIRST pass of a session, so auto-replication never competes
/// with app start (receiver boot, initial scan, first render). The monitor loop's
/// 3 s startup deferral is the same idea, scaled to a background bulk pull.
const COLLAB_AUTO_SYNC_STARTUP_DELAY: std::time::Duration = std::time::Duration::from_secs(90);

/// Armed once per process. The worker is spawned from EVERY `ensure_started`
/// site (autostart + the dev `start_sync`), exactly like the sender resurrection
/// and the orphan sweep, so this flag — not the call site — is what makes it
/// one loop per app run.
static AUTO_SYNC_ARMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// What one pass did — the loop's log line and the unit tests' oracle.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AutoSyncPassOutcome {
    /// Projects whose need set went to the fetch.
    pub projects: usize,
    /// Frames handed to the fetch.
    pub attempted: usize,
    /// Frames landed (fetched or linked, P24).
    pub landed: usize,
    /// Frames that failed, plus one per project whose whole fetch errored.
    pub failed: usize,
    /// Frames parked until the collab store's GC drops a dead entry (P20).
    pub awaiting_gc: usize,
    /// Full holder reports sent by the maintenance a forced pass runs first.
    pub reported: usize,
}

/// May this device replicate a project's frames at all? `coordinator ||
/// data_role == "send_receive"` against the CACHED project row. The hub
/// filters holder rows by the same rule, so a stale cache costs at most one
/// fetch whose holds the hub drops.
fn role_allows_replication(data_role: &str, is_coordinator: bool) -> bool {
    is_coordinator || data_role == "send_receive"
}

/// One replication pass (spec §5.3).
///
/// `scope` limits it to one project ("Sync now"); `None` sweeps every live
/// project (a lost project is never listed, R14). A `Forced` pass first runs
/// [`run_maintenance`] for its scope (disk truth, loss guard, full holder
/// report); a `Fetch` pass never walks the disk — that is the maintenance
/// loop's job (R18). Per project: [`frame_need`] under the project's policy,
/// the role gate, the toggle (`Forced` turns it on; the role gate is
/// authorization and never is) and the pause, then `fetch` — production
/// binds [`fetch_frames`]; tests inject a recorder.
///
/// No Collaboration root (or one that is not reachable) ⇒ one `warn!` and
/// every project is skipped (P25, m5). Never returns an error: signed out,
/// an unreadable catalog, a failing project are each logged and stepped
/// over, because the caller is a loop that must survive all of them.
async fn run_auto_sync_pass<F, Fut>(
    ctx: &ServiceContext,
    kind: PassKind,
    scope: Option<&str>,
    emitter: Option<&dyn ProgressEmitter>,
    fetch: F,
) -> AutoSyncPassOutcome
where
    F: Fn(String, Vec<LocalFrameRow>) -> Fut,
    Fut: std::future::Future<Output = Result<FetchOutcome, ApiError>>,
{
    let mut outcome = AutoSyncPassOutcome::default();

    match crate::api::account::hub_credentials(ctx) {
        Ok(Some(_)) => {}
        Ok(None) => {
            tracing::debug!("collab auto-sync: signed out; pass skipped");
            return outcome;
        }
        Err(e) => {
            tracing::warn!(error = %format!("{e}"), "collab auto-sync: account read failed; pass skipped");
            return outcome;
        }
    }
    if mounted_collaboration_root(ctx).is_none() {
        return outcome;
    }
    if kind == PassKind::Forced {
        outcome.reported = run_maintenance(ctx, scope, emitter).await.reported;
    }

    let projects = {
        let database = match db(ctx) {
            Ok(database) => database,
            Err(e) => {
                tracing::warn!(error = %format!("{e}"), "collab auto-sync: catalog unavailable; pass skipped");
                return outcome;
            }
        };
        match crate::db::collab::list_projects(&database.conn()) {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "collab auto-sync: project list failed; pass skipped");
                return outcome;
            }
        }
    };

    for project in projects {
        if scope.is_some_and(|only| only != project.project_id) {
            continue;
        }
        let pid = project.project_id.clone();
        let role_allows = role_allows_replication(&project.data_role, project.is_coordinator);
        let auto_on = kind == PassKind::Forced || project.auto_replicate;
        let paused = project.replication_paused;
        let rows = {
            let database = match db(ctx) {
                Ok(database) => database,
                Err(e) => {
                    tracing::warn!(error = %format!("{e}"), "collab auto-sync: catalog unavailable; pass aborted");
                    return outcome;
                }
            };
            match crate::db::collab_frames::list_for_project(&database.conn(), &pid) {
                Ok(rows) => rows,
                Err(e) => {
                    tracing::warn!(project_id = %pid, error = %format!("{e:#}"), "collab auto-sync: frame list failed; project skipped this pass");
                    continue;
                }
            }
        };
        let need = frame_need(&rows, &read_policy(&project), role_allows, auto_on, paused);
        if need.is_empty() {
            tracing::debug!(
                project_id = %pid,
                role_allows,
                auto_on,
                paused,
                "collab auto-sync: nothing to replicate"
            );
            continue;
        }
        outcome.projects += 1;
        outcome.attempted += need.len();
        tracing::info!(project_id = %pid, count = need.len(), "collab auto-sync: replicating missing frames");
        match fetch(pid.clone(), need).await {
            Ok(f) => {
                outcome.landed += f.landed;
                outcome.failed += f.failed;
                outcome.awaiting_gc += f.awaiting_gc;
            }
            Err(e) => {
                outcome.failed += 1;
                tracing::warn!(project_id = %pid, error = %e, "collab auto-sync: replication fetch failed; continuing");
            }
        }
    }

    tracing::info!(
        pass = kind.as_str(),
        projects = outcome.projects,
        attempted = outcome.attempted,
        landed = outcome.landed,
        failed = outcome.failed,
        awaiting_gc = outcome.awaiting_gc,
        "collab auto-sync pass complete"
    );
    outcome
}

/// One pass with the REAL fetch bound (the production seam).
pub(crate) async fn replication_pass(
    ctx: &ServiceContext,
    sync: &crate::sync::SyncRuntime,
    kind: PassKind,
    scope: Option<&str>,
    emitter: Option<&dyn ProgressEmitter>,
) -> AutoSyncPassOutcome {
    let forced = kind == PassKind::Forced;
    run_auto_sync_pass(
        ctx,
        kind,
        scope,
        emitter,
        move |project_id, need| async move {
            fetch_frames(ctx, sync, &project_id, need, forced, emitter).await
        },
    )
    .await
}

/// [`replication_pass`] over owned handles, so the loop can run it on its
/// own task.
async fn auto_sync_pass(
    ctx: Arc<ServiceContext>,
    sync: Arc<crate::sync::SyncRuntime>,
    emitter: Option<Arc<dyn ProgressEmitter>>,
    scope: Option<String>,
    kind: PassKind,
) -> AutoSyncPassOutcome {
    replication_pass(&ctx, &sync, kind, scope.as_deref(), emitter.as_deref()).await
}

/// One tick of the maintenance loop (R18): [`run_maintenance`] over every
/// live project, then a kick of the pass loop when frames became fetchable
/// (a replica found missing, a parked one collected).
async fn maintenance_tick(ctx: Arc<ServiceContext>, emitter: Option<Arc<dyn ProgressEmitter>>) {
    let m = run_maintenance(&ctx, None, emitter.as_deref()).await;
    if m.missing + m.cleared > 0 {
        auto_sync_kick().notify_one();
    }
}

/// The auto-replication worker: three loops, each tick on its own task, so a
/// panic anywhere below is logged and the loops survive it (a background
/// loop that dies is a feature that silently stops).
///
/// - the version poll (R19, ruling R15), every
///   [`COLLAB_VERSION_POLL_INTERVAL`], kicks the pass when a project moved;
/// - the maintenance loop (ruling R18), every `interval`: disk truth, the
///   loss guard, the full holder report — never behind a long fetch, so the
///   hub's 75-minute holder freshness always holds;
/// - the pass loop: a [`PassKind::Fetch`] pass every `interval` (the retry
///   cadence) or as soon as a kick arrives.
pub async fn run_collab_auto_sync_loop(
    ctx: Arc<ServiceContext>,
    sync: Arc<crate::sync::SyncRuntime>,
    emitter: Option<Arc<dyn ProgressEmitter>>,
    interval: std::time::Duration,
) {
    tracing::info!(
        interval_secs = interval.as_secs(),
        poll_secs = COLLAB_VERSION_POLL_INTERVAL.as_secs(),
        "collab auto-sync loop armed"
    );
    tokio::spawn(tick_loop(
        COLLAB_VERSION_POLL_STARTUP_DELAY,
        COLLAB_VERSION_POLL_INTERVAL,
        {
            let ctx = Arc::clone(&ctx);
            let emitter = emitter.clone();
            move || {
                let tick = tokio::spawn(version_poll_tick(Arc::clone(&ctx), emitter.clone()));
                async move {
                    if let Err(error) = tick.await {
                        tracing::error!(%error, "collab version poll task panicked");
                    }
                }
            }
        },
    ));
    tokio::spawn(tick_loop(
        COLLAB_AUTO_SYNC_STARTUP_DELAY.min(interval),
        interval,
        {
            let ctx = Arc::clone(&ctx);
            let emitter = emitter.clone();
            move || {
                let tick = tokio::spawn(maintenance_tick(Arc::clone(&ctx), emitter.clone()));
                async move {
                    if let Err(error) = tick.await {
                        tracing::error!(%error, "collab maintenance task panicked");
                    }
                }
            }
        },
    ));
    auto_sync_loop_inner(
        COLLAB_AUTO_SYNC_STARTUP_DELAY.min(interval),
        interval,
        auto_sync_kick(),
        move || {
            let ctx = Arc::clone(&ctx);
            let sync = Arc::clone(&sync);
            let emitter = emitter.clone();
            async move {
                let pass = tokio::spawn(auto_sync_pass(ctx, sync, emitter, None, PassKind::Fetch));
                if let Err(error) = pass.await {
                    tracing::error!(%error, "collab auto-sync pass task panicked");
                }
            }
        },
    )
    .await
}

/// The pass loop's shape, with the pass and the kick injected (the
/// production binding is [`run_collab_auto_sync_loop`] with
/// [`auto_sync_kick`]; tests pass a counter and their own `Notify`).
///
/// The startup grace is deliberately NOT interruptible: it exists so bulk pulls
/// don't compete with app start (receiver boot, initial scan, first render). A
/// kick that lands during a running pass is not lost:
/// [`tokio::sync::Notify::notify_one`] stores ONE permit when nobody is waiting,
/// so the wait below returns immediately the next time round and produces
/// exactly one follow-up pass no matter how many kicks arrived. A permit
/// stored BEFORE a timer pass (e.g. a version move during the grace) is
/// drained when that pass starts — the pass about to run already covers it,
/// so it must not buy a second pass right after. Overlapping per-project work
/// between a pass and a "Sync now" is prevented by the fetch's per-project
/// claim, not by the cadence.
async fn auto_sync_loop_inner<F, Fut>(
    startup_delay: std::time::Duration,
    interval: std::time::Duration,
    kick: &tokio::sync::Notify,
    run_pass: F,
) where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    tokio::time::sleep(startup_delay).await;
    let mut timer_pass = true;
    loop {
        if timer_pass {
            // Polling `notified()` once consumes a stored permit, if any.
            let _ = tokio::time::timeout(std::time::Duration::ZERO, kick.notified()).await;
        }
        run_pass().await;
        tokio::select! {
            _ = tokio::time::sleep(interval) => timer_pass = true,
            _ = kick.notified() => {
                tracing::debug!("collab auto-sync: a hub change arrived; running a pass now");
                timer_pass = false;
            }
        }
    }
}

/// Arm the auto-replication loop for this process (D3 §3.3). Called from every
/// `ensure_started` site; the second and later calls are no-ops, so the app runs
/// exactly one worker. Returns the spawned handle only for the call that armed
/// it (tests / callers that want to observe it).
pub fn spawn_collab_auto_sync(
    ctx: Arc<ServiceContext>,
    sync: Arc<crate::sync::SyncRuntime>,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Option<tokio::task::JoinHandle<()>> {
    if AUTO_SYNC_ARMED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        tracing::debug!("collab auto-sync already armed; not spawning a second loop");
        return None;
    }
    // Task 10: the coalesced auto-publish worker shares this arming guard
    // rather than getting its own — the first (and only) caller to win the
    // swap above arms BOTH loops for the process. Render+solver-gated (it
    // drives `publish_collab_frames`); this module stays ungated, so the
    // call is `cfg`-scoped here rather than at every `ensure_started` site.
    #[cfg(all(feature = "render", feature = "solver"))]
    crate::api::collab_autopublish::spawn_auto_publish_worker(Arc::clone(&ctx), emitter.clone());
    Some(tokio::spawn(run_collab_auto_sync_loop(
        ctx,
        sync,
        emitter,
        COLLAB_AUTO_SYNC_INTERVAL,
    )))
}

/// Set one project's auto-replication preference (D3 §3.3). Local-only — the hub
/// never learns of it. The worker reads the column at the start of every pass;
/// turning it off also stops a running fetch after its current batch (R19).
pub fn set_project_auto_replicate(
    ctx: &ServiceContext,
    project_id: &str,
    enabled: bool,
) -> Result<(), ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let updated = crate::db::collab::set_auto_replicate(&conn, project_id, enabled)?;
    if updated == 0 {
        return Err(ApiError::Invalid(format!("unknown project {project_id}")));
    }
    tracing::info!(project_id, enabled, "collab auto-replication toggled");
    if !enabled {
        cancel_project_fetch(ctx, project_id);
    }
    Ok(())
}

/// "Sync now" for one project: the version poll (so the pass works from the
/// current manifest), then ONE [`PassKind::Forced`] pass scoped to
/// `project_id`, on a spawned task — the command returns as soon as the
/// project is known. The pass walks the disk, reports holders and fetches
/// with the auto-replicate toggle forced on (an explicit user act); the role
/// gate is authorization and is never forced. Outcomes ride the pass's
/// events (`collab-frames-landed`, `collab-replication-paused`).
///
/// A project the poll moved besides this one gets the usual kick, since this
/// pass only covers `project_id`.
pub fn sync_project_now(
    ctx: Arc<ServiceContext>,
    sync: Arc<crate::sync::SyncRuntime>,
    project_id: &str,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Result<(), ApiError> {
    {
        let db = db(&ctx)?;
        live_project(&db.conn(), project_id)?;
    }
    tracing::info!(project_id, "collab sync now requested");
    let scope = project_id.to_string();
    tokio::spawn(async move {
        #[cfg(all(feature = "render", feature = "solver"))]
        match poll_versions_once(&ctx, emitter.as_deref()).await {
            Ok(moved) => {
                if moved.iter().any(|id| id != &scope) {
                    kick_if_versions_moved(&moved);
                }
            }
            Err(e) => {
                tracing::warn!(project_id = %scope, error = %e, "sync now: version poll failed; syncing from the cached manifest")
            }
        }
        #[cfg(not(all(feature = "render", feature = "solver")))]
        {
            // Headless: no project refresh, so no poll; the manifest delta
            // alone (logged inside on failure).
            let _ = sync_manifest(&ctx, &scope, emitter.as_deref(), None).await;
        }
        auto_sync_pass(ctx, sync, emitter, Some(scope), PassKind::Forced).await;
    });
    Ok(())
}

// ── Project-scoped WBPP export (slice 5, "processor payoff") ──────────────────

/// Load the WBPP config from settings, or the default when absent. A private
/// reproduction of the byte-identical `load_wbpp_config` in both host crates
/// (`commands/export.rs` / `routes/export.rs`) — the runner is core-resident and
/// must not depend on a host. Absent row ⇒ default; a parse failure ⇒ `warn!` +
/// `Err` (callers use `.unwrap_or_default()`, exactly like both hosts).
fn load_wbpp_config(conn: &rusqlite::Connection) -> Result<WbppExportConfig> {
    let result: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            rusqlite::params!["export.wbpp_config"],
            |row| row.get(0),
        )
        .ok();
    match result {
        Some(json) => serde_json::from_str(&json).map_err(|e| {
            tracing::warn!(error = %e, "failed to parse WBPP config");
            anyhow!(e)
        }),
        None => Ok(WbppExportConfig::default()),
    }
}

/// The project export: collect (Task 1) → organize one folder tree per publisher
/// under `<output_dir>/<sanitized project title>/`, with each dataset's
/// `frame_set_name` = the publisher display (Д2 — one organizer call per
/// publisher). Rides the standard export events with the Д3 sentinel
/// `frame_set_id = -1`, registering its cancel flag under that key exactly like
/// the frame-set export.
///
/// Ungated: the collector is a pure catalog read and the organizer is reused
/// untouched, so this compiles in the headless build.
///
/// Progress limitation (accepted): each organizer call emits percent against ITS
/// OWN dataset total, so the bar restarts per publisher — the Task-2 dialog
/// shows current file + publisher count, not one monotonic percent.
pub async fn export_project_for_wbpp(
    ctx: &ServiceContext,
    project_id: &str,
    output_dir: &str,
    use_symlinks: bool,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Result<crate::export::models::ExportResult, ApiError> {
    use crate::export::models::{
        sanitize_display_folder_name, ExportCompleteEvent, ExportProgressEvent, ExportResult,
    };
    use std::sync::atomic::{AtomicBool, Ordering};

    const SENTINEL: i64 = -1;

    // Register the cancel flag under the sentinel (the registry is core-resident).
    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut exports = ctx
            .active_exports
            .lock()
            .map_err(|e| ApiError::Internal(format!("active_exports lock poisoned: {e}")))?;
        exports.insert(
            SENTINEL,
            crate::services::ExportHandle {
                cancel_flag: cancel_flag.clone(),
            },
        );
    }

    // The whole export runs inside a scoped block so the terminal event AND the
    // deregister below fire on every path (success, collector error, organize
    // error) — never swallowed.
    let outcome: Result<ExportResult, ApiError> = async {
        if let Some(e) = emitter.as_deref() {
            crate::events::emit_event(
                e,
                "export-progress",
                &ExportProgressEvent {
                    frame_set_id: SENTINEL,
                    current: 0,
                    total: 0,
                    percent: 0.0,
                    current_file: None,
                    phase: "collecting".to_string(),
                },
            );
        }

        // Resolve own_display (Д2) + the WBPP config with the DB borrow scoped out
        // before the spawn_blocking/await.
        // The device key is IDENTITY — always the identity dir.
        let identity_dir = crate::api::sync::sync_dirs(ctx)?.identity_dir;
        let own_node = DeviceKey::load_or_create(&device_key_path(&identity_dir))
            .map_err(|e| ApiError::Internal(format!("device key: {e:#}")))?
            .node_id();
        let (own_display, config) = {
            let db = db(ctx)?;
            let conn = db.conn();
            let members: Vec<SnapshotMember> = crate::db::collab::get_project(&conn, project_id)?
                .map(|p| serde_json::from_str(&p.members_json).unwrap_or_default())
                .unwrap_or_default();
            let mut display = own_display_name(&members, &own_node);
            if display.is_empty() {
                tracing::warn!(
                    project_id,
                    "project export: could not resolve own display name from snapshot; using \"own\""
                );
                display = "own".to_string();
            }
            (display, load_wbpp_config(&conn).unwrap_or_default())
        };

        // Collect on a blocking thread (pure catalog read; own DB handle).
        let data = {
            let db_handle = db(ctx)?.clone();
            let pid = project_id.to_string();
            let own = own_display.clone();
            tokio::task::spawn_blocking(
                move || -> Result<crate::export::ProjectExportData> {
                    let conn = db_handle.conn();
                    crate::export::collect_project_export_data(&conn, &pid, &own)
                },
            )
            .await
            .map_err(|e| ApiError::Internal(format!("collect join error: {e}")))??
        };

        // <output_dir>/<sanitized project title>/, then one organizer call per
        // publisher (the organizer joins the publisher folder itself).
        let title_dir = Path::new(output_dir).join(sanitize_display_folder_name(&data.title));
        let mut files_organized = 0i32;
        // Prepend the collector's per-skip notes (a frame row whose manifest
        // metadata is unreadable) so omitted frames reach
        // ExportResult.warnings — and the dialog — rather than vanishing behind
        // a smaller "N files organized" count.
        let mut warnings: Vec<String> = data.warnings.clone();
        let mut cancelled = false;
        for (publisher, dataset) in &data.publishers {
            if cancel_flag.load(Ordering::Relaxed) {
                cancelled = true;
                break;
            }
            let result = crate::export::file_organizer::organize_files_wbpp(
                &title_dir,
                dataset,
                use_symlinks,
                &config,
                emitter.as_deref(),
                SENTINEL,
                &cancel_flag,
                // Collab datasets are always copied — project frames are already
                // calibrated, never marked for generation.
                None,
                Some(&ctx.image_pool),
            )
            .map_err(|e| ApiError::Internal(format!("organize publisher {publisher}: {e:#}")))?;
            files_organized += result.files_organized;
            warnings.extend(result.warnings);
        }
        if cancelled || cancel_flag.load(Ordering::Relaxed) {
            return Ok(ExportResult {
                success: false,
                output_dir: output_dir.to_string(),
                files_organized,
                scripts_generated: Vec::new(),
                warnings,
                error: Some("Export cancelled".to_string()),
            });
        }

        Ok(ExportResult {
            success: true,
            output_dir: output_dir.to_string(),
            files_organized,
            scripts_generated: Vec::new(),
            warnings,
            error: None,
        })
    }
    .await;

    // Deregister on every path.
    if let Ok(mut exports) = ctx.active_exports.lock() {
        exports.remove(&SENTINEL);
    }

    // Terminal event (success or failure), never swallowed.
    if let Some(e) = emitter.as_deref() {
        let complete = match &outcome {
            Ok(r) => ExportCompleteEvent {
                frame_set_id: SENTINEL,
                success: r.success,
                files_organized: r.files_organized,
                warnings: r.warnings.clone(),
                error: r.error.clone(),
                output_dir: output_dir.to_string(),
            },
            Err(err) => ExportCompleteEvent {
                frame_set_id: SENTINEL,
                success: false,
                files_organized: 0,
                warnings: Vec::new(),
                error: Some(err.to_string()),
                output_dir: output_dir.to_string(),
            },
        };
        crate::events::emit_event(e, "export-complete", &complete);
    }

    match &outcome {
        Ok(r) => tracing::info!(
            project_id,
            files_organized = r.files_organized,
            outcome = if r.success { "ok" } else { "cancelled" },
            "project export finished"
        ),
        Err(err) => tracing::error!(project_id, error = %err, "project export failed"),
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::collab::{upsert_project, CollabProjectRow};
    use crate::sharing::iroh::node::Role;
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;
    use rusqlite::Connection;
    use std::time::Duration;

    const NODE_COORD: NodeId = [0xA1; 32]; // coordinator + send_receive
    const NODE_SEND_ONLY: NodeId = [0xB2; 32]; // send-only contributor
    const NODE_SR: NodeId = [0xC3; 32]; // send_receive, non-coordinator

    fn hash_bytes(bytes: &[u8]) -> String {
        format!("{:016x}", xxhash_rust::xxh3::xxh3_64(bytes))
    }

    /// A three-member snapshot (encoded exactly as slice-3 caches it): a
    /// coordinator+`send_receive` (NODE_COORD), a send-only contributor
    /// (NODE_SEND_ONLY), and a non-coordinator `send_receive` (NODE_SR).
    fn members_json() -> String {
        serde_json::json!([
            {"accountId":"acc-coord","displayName":"Coord","dataRole":"send_receive","coordinator":true,"nodes":[B64.encode(NODE_COORD)]},
            {"accountId":"acc-so","displayName":"SendOnly","dataRole":"send","coordinator":false,"nodes":[B64.encode(NODE_SEND_ONLY)]},
            {"accountId":"acc-sr","displayName":"SendRecv","dataRole":"send_receive","coordinator":false,"nodes":[B64.encode(NODE_SR)]}
        ])
        .to_string()
    }

    fn seed_project(conn: &Connection, project_id: &str, members_json: &str) {
        upsert_project(
            conn,
            &CollabProjectRow {
                project_id: project_id.to_string(),
                slug: format!("{project_id}-slug"),
                title: "T".to_string(),
                data_role: "send_receive".to_string(),
                is_coordinator: true,
                require_approval: false,
                pending_frames: 0,
                project_status: "active".to_string(),
                target_name: "M42".to_string(),
                target_ra_deg: 83.8,
                target_dec_deg: -5.4,
                target_radius_deg: 1.0,
                membership_version: 1,
                snapshot_payload_b64: "x".to_string(),
                snapshot_signature_b64: "x".to_string(),
                members_json: members_json.to_string(),
                thresholds_version: None,
                thresholds_rules_json: None,
                gov_caps_json: "[]".into(),
                // all ignored on write (local preference / sync-state / dictionary)
                auto_replicate: true,
                synced_caps_json: "[]".into(),
                hub_version: 0,
                manifest_cursor: 0,
                dictionary_version: None,
                dictionary_json: None,
                policy_json: r#"{"mode":"all"}"#.into(),
                replication_paused: false,
                auto_publish: true,
                fetched_at: String::new(),
            },
        )
        .unwrap();
    }

    // ── Project export runner (P26: paths from `project_frames_local`) ───────

    /// The project export runner lays a per-publisher WBPP tree under the
    /// project title and copies the replica's landed FITS byte-exact:
    /// `<out>/<title>/<publisher>/camera_<instrume>/lights/<basename>`. The
    /// file is found through the row's `landed_path`, its metadata read from
    /// the manifest row.
    #[tokio::test]
    async fn export_project_lays_per_publisher_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));

        // A real tiny FITS as the landed replica file.
        let landed = tmp.path().join("Collab").join("Alice").join("L_0001.fits");
        std::fs::create_dir_all(landed.parent().unwrap()).unwrap();
        let pixels = vec![0.25f32, 0.5, 0.75, 1.0];
        crate::fits_writer::write_fits_f32(&landed, 2, 2, 1, &pixels, &[]).unwrap();

        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            seed_project(&conn, "p-1", "[]");
            let view: crate::collab::hub_client::FrameViewWire =
                serde_json::from_value(serde_json::json!({
                    "frameUuid": "u-1", "publisherAccountId": "acc-alice",
                    "publisherDisplayName": "Alice", "own": false, "fileName": "L_0001.fits",
                    "contentVersion": 1, "blake3": "b".repeat(64), "byteSize": 1,
                    "xxh3": "0123456789abcdef", "filterRaw": "L", "filterCanonical": "L",
                    "channel": "mono", "exptimeSec": 300.0, "meta": {"instrume": "CamA"},
                    "gateVersion": 0, "accepted": true, "state": "published",
                    "manifestVersion": 1, "createdAt": "2026-07-13T00:00:00Z", "holderCount": 1
                }))
                .unwrap();
            crate::db::collab_frames::upsert_from_manifest(&conn, "p-1", &view).unwrap();
            crate::db::collab_frames::set_landed(
                &conn,
                "p-1",
                "u-1",
                &landed.to_string_lossy(),
                "1:1",
            )
            .unwrap();
        }

        let out = tmp.path().join("out");
        let result = export_project_for_wbpp(&ctx, "p-1", &out.to_string_lossy(), false, None)
            .await
            .unwrap();
        assert!(result.success, "export succeeded: {:?}", result.error);
        assert_eq!(result.files_organized, 1, "one replica frame organized");

        // seed_project titles the project "T"; instrume "CamA" → camera_cama.
        let dest = out
            .join("T")
            .join("Alice")
            .join("camera_cama")
            .join("lights")
            .join("L_0001.fits");
        assert!(dest.exists(), "expected {dest:?}");
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            std::fs::read(&landed).unwrap(),
            "the organized copy is byte-identical to the landed replica"
        );
    }

    /// `list_project_frames` (wave 2 Task 11) reads the reliable local columns
    /// AND parses `meta.fwhmArcsec`/`meta.eccentricity`/`meta.starsDetected`
    /// out of the retained manifest row (`build_frame_meta`'s camelCase keys)
    /// — the metrics the frames table shows without a second query. Scoped to
    /// the requested project, like every other cache-only list view.
    #[test]
    fn list_project_frames_reads_metrics_from_meta() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        let view: crate::collab::hub_client::FrameViewWire = serde_json::from_value(serde_json::json!({
            "frameUuid": "u-1", "publisherAccountId": "acc-alice", "publisherDisplayName": "Alice",
            "own": false, "fileName": "c_u-1.fits", "contentVersion": 1, "blake3": "b".repeat(64),
            "byteSize": 4096, "xxh3": "0123456789abcdef", "filterRaw": "Red", "filterCanonical": "R",
            "channel": "mono", "exptimeSec": 300.0, "dateObs": "2026-07-01T21:00:00Z",
            "meta": {"fwhmArcsec": 2.4, "eccentricity": 0.35, "starsDetected": 512},
            "gateVersion": 0, "accepted": true, "state": "published", "manifestVersion": 1,
            "createdAt": "2026-07-13T00:00:00Z", "holderCount": 2
        }))
        .unwrap();
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            seed_project(&conn, "p-1", &members_json());
            seed_project(&conn, "p-OTHER", &members_json());
            crate::db::collab_frames::upsert_from_manifest(&conn, "p-1", &view).unwrap();
            // A different project must not leak into the list.
            let mut other = view.clone();
            other.frame_uuid = "u-2".into();
            crate::db::collab_frames::upsert_from_manifest(&conn, "p-OTHER", &other).unwrap();
        }

        let views = list_project_frames(&ctx, "p-1").unwrap();
        assert_eq!(views.len(), 1, "scoped to p-1");
        let f = &views[0];
        assert_eq!(f.frame_uuid, "u-1");
        assert_eq!(f.file_name, "c_u-1.fits");
        assert_eq!(f.publisher, "Alice");
        assert!(!f.own);
        assert_eq!(f.filter, "R");
        assert_eq!(f.exptime_sec, 300.0);
        assert_eq!(f.date_obs.as_deref(), Some("2026-07-01T21:00:00Z"));
        assert_eq!(f.state, "published");
        assert!(f.accepted);
        assert_eq!(f.holder_count, 2);
        assert_eq!(f.byte_size, 4096);
        assert_eq!(f.content_version, 1);
        assert_eq!(f.fwhm_arcsec, Some(2.4), "parsed from meta.fwhmArcsec");
        assert_eq!(f.eccentricity, Some(0.35), "parsed from meta.eccentricity");
        assert_eq!(
            f.stars_detected,
            Some(512),
            "parsed from meta.starsDetected"
        );
    }

    async fn wait_until<F: FnMut() -> bool>(mut pred: F, timeout: Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if pred() {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("wait_until timed out after {timeout:?}");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// A minimal file-backed-`Database` [`ServiceContext`] (no keychain), copied
    /// from `api::sync` / `api::collab` tests. A tempdir-FILE-backed `Database`
    /// (not `:memory:`) so the pool + the receiver's own `CatalogSyncStore` see
    /// one catalog file.
    fn test_ctx() -> (tempfile::TempDir, ServiceContext) {
        use crate::cache::MemoryImageCache;
        use crate::services::compute_queue::ComputeQueue;
        use crate::services::operation_queue::OperationQueue;
        use crate::settings::SettingsManager;
        use std::collections::HashMap;
        #[cfg(all(feature = "render", feature = "solver"))]
        use std::sync::RwLock;
        use std::sync::{Mutex, OnceLock};

        let tmp = tempfile::tempdir().unwrap();
        let database = crate::db::Database::new(tmp.path().join("catalog.db")).unwrap();
        let db_cell = OnceLock::new();
        let _ = db_cell.set(database);
        let ctx = ServiceContext {
            db: db_cell,
            settings: Arc::new(SettingsManager::new()),
            memory_cache: Arc::new(Mutex::new(MemoryImageCache::new(10, 5))),
            active_scans: Arc::new(Mutex::new(HashMap::new())),
            active_exports: Arc::new(Mutex::new(HashMap::new())),
            active_analyses: Arc::new(Mutex::new(HashMap::new())),
            active_plate_solves: Arc::new(Mutex::new(HashMap::new())),
            active_archives: Arc::new(Mutex::new(HashMap::new())),
            active_master_builds: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(all(feature = "render", feature = "solver"))]
            active_stacks: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(all(feature = "render", feature = "solver"))]
            dso_catalog: Arc::new(RwLock::new(None)),
            image_pool: Arc::new(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(1)
                    .build()
                    .unwrap(),
            ),
            operation_queue: OperationQueue::start(),
            compute_queue: ComputeQueue::new(),
            iroh_node: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
        };
        (tmp, ctx)
    }

    /// Point `ctx`'s account hub at `uri` + store a device token (mirrors
    /// `api::collab::wire_hub`).
    fn wire_hub(ctx: &ServiceContext, uri: &str) {
        {
            let conn = db(ctx).unwrap().conn();
            crate::db::set_setting(&conn, crate::settings::keys::ACCOUNT_HUB_URL, uri).unwrap();
        }
        crate::api::account::store_token_for_test(ctx, "tok").unwrap();
    }

    /// This device's node id for `ctx`'s sync dir — the identity a member's
    /// snapshot entry names.
    fn own_node_for(ctx: &ServiceContext) -> NodeId {
        let identity_dir = crate::api::sync::sync_dirs(ctx).unwrap().identity_dir;
        DeviceKey::load_or_create(&device_key_path(&identity_dir))
            .unwrap()
            .node_id()
    }

    /// Total bytes a node's endpoint has sent since bind (relay + direct) — the
    /// per-provider oracle, taken from iroh's own socket counters. The Split
    /// progress stream is lossy upstream (T1's re-gate), so telemetry can never
    /// answer "did this provider serve payload"; this can.
    fn sent_bytes(node: &Arc<crate::sharing::iroh::node::SharedIrohNode>) -> u64 {
        let c = node.counters_snapshot_for_test();
        c.send_direct_bytes.saturating_add(c.send_relay_bytes)
    }

    /// A provider's send delta must clear this to count as "served real payload".
    /// Measured non-serving providers still send ~16 KB of handshake/ACK
    /// traffic; one served 96 KiB child is an order of magnitude above that.
    const SERVED_PAYLOAD_FLOOR: u64 = 64 * 1024;

    /// The pass's role gate: `coordinator || data_role == "send_receive"`.
    #[test]
    fn role_allows_replication_matches_the_download_guard() {
        assert!(role_allows_replication("send_receive", false));
        assert!(
            role_allows_replication("send", true),
            "a coordinator may always pull"
        );
        assert!(!role_allows_replication("send", false));
    }

    /// The kick actually WAKES the loop — spec §3.3 promises a pass right
    /// after a poll saw a project move, and without it a change waits out the
    /// 20-minute cadence. The interval here is an hour, so only a kick can
    /// produce the second pass.
    #[tokio::test]
    async fn a_kick_wakes_the_auto_sync_loop() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let passes = Arc::new(AtomicUsize::new(0));
        let task = tokio::spawn({
            let passes = Arc::clone(&passes);
            async move {
                auto_sync_loop_inner(
                    Duration::ZERO,
                    Duration::from_secs(3600),
                    auto_sync_kick(),
                    move || {
                        let passes = Arc::clone(&passes);
                        async move {
                            passes.fetch_add(1, Ordering::SeqCst);
                        }
                    },
                )
                .await
            }
        });

        wait_until(
            || passes.load(Ordering::SeqCst) >= 1,
            Duration::from_secs(5),
        )
        .await;
        auto_sync_kick().notify_one();
        wait_until(
            || passes.load(Ordering::SeqCst) >= 2,
            Duration::from_secs(5),
        )
        .await;
        task.abort();
    }

    /// The api-level toggle persists through the db accessor pair.
    #[test]
    fn set_project_auto_replicate_persists() {
        let (_tmp, ctx) = test_ctx();
        {
            let conn = db(&ctx).unwrap().conn();
            seed_project(&conn, "p-auto", &members_json());
        }

        let toggle = |ctx: &ServiceContext| {
            let conn = db(ctx).unwrap().conn();
            crate::db::collab::get_project(&conn, "p-auto")
                .unwrap()
                .unwrap()
                .auto_replicate
        };

        set_project_auto_replicate(&ctx, "p-auto", false).unwrap();
        assert!(!toggle(&ctx));

        set_project_auto_replicate(&ctx, "p-auto", true).unwrap();
        assert!(toggle(&ctx));

        assert!(
            set_project_auto_replicate(&ctx, "no-such-project", false).is_err(),
            "an unknown project is a user-visible error, not a silent no-op"
        );
    }

    // ── Task 8 (wave 2): version poll + manifest delta, against the fake hub ──

    /// Shared fixture for the `poll` / `manifest` tests: one app context signed
    /// in to ONE stateful fake hub as `acc-me`, plus a second member `acc-o`.
    #[cfg(all(feature = "render", feature = "solver"))]
    mod v3_fx {
        use super::*;
        pub(super) use crate::collab::fake_hub::FakeHub;
        pub(super) use crate::collab::hub_client::FrameViewWire;
        pub(super) use crate::db::collab_frames::{self as frames_db, FrameOrigin, LocalFrameRow};

        pub(super) const PID: &str = "p1";

        pub(super) struct Fx {
            pub tmp: tempfile::TempDir,
            pub ctx: ServiceContext,
            pub hub: FakeHub,
        }

        /// `acc-me` (this context, token `tok`) is a `role` member; `acc-o` a
        /// `send_receive` member. The project starts at version 1 with the
        /// fake's default dictionary (version 1) and no thresholds.
        pub(super) async fn fx(role: &str, coordinator: bool, require_approval: bool) -> Fx {
            let hub = FakeHub::start().await;
            let (tmp, ctx) = test_ctx();
            wire_hub(&ctx, &hub.uri());
            hub.add_account("tok", "acc-me", "Me", &B64.encode(own_node_for(&ctx)), None);
            hub.add_account("tok-o", "acc-o", "Other", &B64.encode([0x55u8; 32]), None);
            hub.add_project(
                PID,
                "m31",
                &[
                    ("acc-me", role, coordinator),
                    ("acc-o", "send_receive", false),
                ],
                require_approval,
            );
            Fx { tmp, ctx, hub }
        }

        pub(super) fn rows(ctx: &ServiceContext) -> Vec<LocalFrameRow> {
            frames_db::list_for_project(&db(ctx).unwrap().conn(), PID).unwrap()
        }

        pub(super) fn row(ctx: &ServiceContext, uuid: &str) -> Option<LocalFrameRow> {
            frames_db::get(&db(ctx).unwrap().conn(), PID, uuid).unwrap()
        }

        pub(super) fn project(ctx: &ServiceContext) -> Option<CollabProjectRow> {
            crate::db::collab::get_project(&db(ctx).unwrap().conn(), PID).unwrap()
        }

        pub(super) async fn request_count(hub: &FakeHub) -> usize {
            hub.server.received_requests().await.unwrap().len()
        }

        /// `(method, path)` of every request after the first `from`.
        pub(super) async fn requests_since(hub: &FakeHub, from: usize) -> Vec<(String, String)> {
            hub.server.received_requests().await.unwrap()[from..]
                .iter()
                .map(|r| (r.method.to_string(), r.url.path().to_string()))
                .collect()
        }

        /// `(since, after)` of every manifest request after the first `from`.
        pub(super) async fn manifest_queries(
            hub: &FakeHub,
            from: usize,
        ) -> Vec<(i64, Option<String>)> {
            hub.server.received_requests().await.unwrap()[from..]
                .iter()
                .filter(|r| r.url.path().ends_with("/manifest"))
                .map(|r| {
                    let q = |k: &str| {
                        r.url
                            .query_pairs()
                            .find(|(key, _)| key == k)
                            .map(|(_, v)| v.into_owned())
                    };
                    (q("since").unwrap().parse().unwrap(), q("after"))
                })
                .collect()
        }

        /// Records every emitted event.
        #[derive(Default)]
        pub(super) struct RecordingEmitter {
            pub events: std::sync::Mutex<Vec<(String, serde_json::Value)>>,
        }

        impl ProgressEmitter for RecordingEmitter {
            fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
                self.events
                    .lock()
                    .unwrap()
                    .push((event_name.to_string(), payload));
            }
        }

        impl RecordingEmitter {
            /// `(kind, count)` of every `collab-frames-changed` event, sorted.
            pub(super) fn frame_changes(&self) -> Vec<(String, u64)> {
                let mut out: Vec<(String, u64)> = self
                    .events
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(name, _)| name == "collab-frames-changed")
                    .map(|(_, p)| {
                        assert_eq!(p["projectId"], PID);
                        (
                            p["kind"].as_str().unwrap().to_string(),
                            p["count"].as_u64().unwrap(),
                        )
                    })
                    .collect();
                out.sort();
                out
            }
        }

        /// A real relay-disabled node installed on `ctx`, with the
        /// Collaboration root `<tmp>/Collab` mounted. Returns the node and the
        /// stored root.
        pub(super) async fn bind_node_with_collab_root(
            fx: &Fx,
        ) -> (Arc<crate::sharing::iroh::node::SharedIrohNode>, PathBuf) {
            let dirs = crate::api::sync::sync_dirs(&fx.ctx).unwrap();
            std::fs::create_dir_all(&dirs.identity_dir).unwrap();
            std::fs::create_dir_all(&dirs.working_dir).unwrap();
            let node = crate::sharing::iroh::node::SharedIrohNode::bind_with(
                &dirs.identity_dir,
                &dirs.working_dir,
                iroh::RelayMode::Disabled,
                crate::sharing::iroh::node::NodeOptions::default(),
            )
            .await
            .expect("bind relay-disabled node");
            *fx.ctx.iroh_node.lock().await = Some(Arc::clone(&node));
            let requested = fx.tmp.path().join("Collab");
            std::fs::create_dir_all(&requested).unwrap();
            let collab = PathBuf::from(
                crate::api::scan_roots::set_collaboration_dir(
                    &fx.ctx,
                    requested.to_string_lossy().to_string(),
                    &crate::api::PathPolicy::AllowAll,
                )
                .await
                .unwrap(),
            );
            assert!(node.collab_store().is_some(), "the collab store is mounted");
            (node, collab)
        }

        /// Tags under `<prefix>` in the collab store.
        pub(super) async fn collab_tags(
            node: &crate::sharing::iroh::node::SharedIrohNode,
            prefix: &str,
        ) -> usize {
            use n0_future::StreamExt as _;
            let store = node.collab_store().expect("collab store mounted");
            let mut stream = store.tags().list_prefix(prefix.as_bytes()).await.unwrap();
            let mut n = 0;
            while let Some(item) = stream.next().await {
                item.unwrap();
                n += 1;
            }
            n
        }

        /// `(level, message)` of every event on THIS thread while the capture
        /// lives (a `#[tokio::test]` runs its body on one thread) — the
        /// scoped-default + custom-`Layer` pattern of `api::masters`' tests.
        pub(super) struct LogCapture {
            seen: Arc<std::sync::Mutex<Vec<(String, String)>>>,
            _guard: tracing::subscriber::DefaultGuard,
        }

        pub(super) fn capture_logs() -> LogCapture {
            use tracing_subscriber::layer::SubscriberExt;

            #[derive(Clone, Default)]
            struct Seen(Arc<std::sync::Mutex<Vec<(String, String)>>>);
            struct Message(String);
            impl tracing::field::Visit for Message {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    if field.name() == "message" {
                        self.0 = format!("{value:?}");
                    }
                }
            }
            impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Seen {
                fn on_event(
                    &self,
                    event: &tracing::Event<'_>,
                    _ctx: tracing_subscriber::layer::Context<'_, S>,
                ) {
                    let mut msg = Message(String::new());
                    event.record(&mut msg);
                    self.0
                        .lock()
                        .unwrap()
                        .push((event.metadata().level().to_string(), msg.0));
                }
            }
            let seen = Seen::default();
            let guard =
                tracing::subscriber::set_default(tracing_subscriber::registry().with(seen.clone()));
            tracing::callsite::rebuild_interest_cache();
            LogCapture {
                seen: seen.0,
                _guard: guard,
            }
        }

        impl LogCapture {
            /// The captured events at `levels`, drained.
            pub(super) fn take(&self, levels: &[&str]) -> Vec<String> {
                std::mem::take(&mut *self.seen.lock().unwrap())
                    .into_iter()
                    .filter(|(level, _)| levels.contains(&level.as_str()))
                    .map(|(_, msg)| msg)
                    .collect()
            }
        }

        /// An own row as the publish path records it (R8 adoption shape when
        /// `state = "unknown"`).
        pub(super) fn own_row(uuid: &str, state: &str, landed: &str) -> LocalFrameRow {
            LocalFrameRow {
                project_id: PID.into(),
                frame_uuid: uuid.into(),
                content_version: 1,
                origin: FrameOrigin::Own,
                publisher_account_id: "acc-me".into(),
                publisher_display: "Me".into(),
                file_name: format!("{uuid}.fits"),
                filter_canonical: "L".into(),
                state: state.into(),
                accepted: true,
                byte_size: 7,
                xxh3: "0".repeat(16),
                blake3: "0".repeat(64),
                holder_count: 0,
                manifest_version: 0,
                manifest_json: "{}".into(),
                landed_path: Some(landed.into()),
                size_mtime_seen: Some("7:1".into()),
                on_disk: true,
                locally_declined: false,
                awaiting_gc: false,
                source_frame_id: Some(42),
                recipe_hash: Some("recipe".into()),
                last_error: None,
                updated_at: String::new(),
            }
        }
    }

    #[cfg(all(feature = "render", feature = "solver"))]
    mod poll {
        use super::v3_fx::*;
        use super::*;
        use crate::api::collab::take_gate_moves_seen;
        use std::sync::atomic::{AtomicUsize, Ordering};

        /// Once the project is joined and synced, a poll where nothing moved
        /// costs exactly ONE hub request: `/me/project-versions`.
        #[tokio::test]
        async fn version_poll_is_quiet_when_nothing_moved() {
            let fx = fx("send_receive", false, false).await;
            fx.hub.seed_frames(PID, "acc-o", &["f1"], "published");

            let moved = poll_versions_once(&fx.ctx, None).await.unwrap();
            assert_eq!(moved, vec![PID.to_string()], "the join moves the project");
            assert_eq!(rows(&fx.ctx).len(), 1);

            for _ in 0..2 {
                let before = request_count(&fx.hub).await;
                let moved = poll_versions_once(&fx.ctx, None).await.unwrap();
                assert!(moved.is_empty(), "nothing moved: {moved:?}");
                assert_eq!(
                    requests_since(&fx.hub, before).await,
                    vec![("GET".to_string(), "/api/v1/me/project-versions".to_string())],
                    "a quiet poll is one request"
                );
            }
        }

        /// A moved version pulls only the delta: the manifest request carries
        /// `since=<old cursor>` and the local rows grow by exactly the new ones.
        #[tokio::test]
        async fn moved_version_pulls_only_the_delta() {
            let fx = fx("send_receive", false, false).await;
            fx.hub
                .seed_frames(PID, "acc-o", &["f1", "f2", "f3"], "published");
            poll_versions_once(&fx.ctx, None).await.unwrap();
            assert_eq!(rows(&fx.ctx).len(), 3);
            let cursor = project(&fx.ctx).unwrap().manifest_cursor;
            assert_eq!(cursor, fx.hub.frame(PID, "f1").unwrap().manifest_version);

            fx.hub.seed_frames(PID, "acc-o", &["f4", "f5"], "published");
            let before = request_count(&fx.hub).await;
            let moved = poll_versions_once(&fx.ctx, None).await.unwrap();
            assert_eq!(moved, vec![PID.to_string()]);
            assert_eq!(
                manifest_queries(&fx.hub, before).await,
                vec![(cursor, None)],
                "one manifest request, from the old cursor"
            );
            assert_eq!(rows(&fx.ctx).len(), 5, "the local rows grow by 2");
            let p = project(&fx.ctx).unwrap();
            assert_eq!(
                p.manifest_cursor,
                fx.hub.frame(PID, "f4").unwrap().manifest_version
            );
            assert_eq!(
                p.hub_version,
                fx.hub.state.lock().unwrap().projects[PID].version,
                "the poll vouches for the listed version"
            );
        }

        /// R14: the project disappears from my lists. It is marked lost and
        /// hidden, its collab-store tags (in-flight ones too) and replica rows
        /// go, my own row and file stay. A re-join clears the mark, refetches
        /// the manifest from 0 and re-binds my own row.
        #[tokio::test]
        async fn lost_project_unseeds_and_keeps_own_files() {
            let fx = fx("send_receive", false, false).await;
            let (node, collab) = bind_node_with_collab_root(&fx).await;
            fx.hub.seed_frames(PID, "acc-me", &["fm"], "published");
            fx.hub.seed_frames(PID, "acc-o", &["fr"], "published");
            poll_versions_once(&fx.ctx, None).await.unwrap();
            assert_eq!(row(&fx.ctx, "fm").unwrap().origin, FrameOrigin::Own);
            assert!(row(&fx.ctx, "fr").is_some());

            // My own file + a landed replica, both seeded; plus an in-flight tag.
            let own_path = collab.join("m31").join("me").join("fm.fits");
            let rep_path = collab.join("m31").join("other").join("fr.fits");
            for (p, byte) in [(&own_path, 1u8), (&rep_path, 2u8)] {
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(p, vec![byte; 32 * 1024]).unwrap();
            }
            let own_hash = node
                .seed_project_frame(PID, "fm", 1, &own_path)
                .await
                .unwrap();
            node.seed_project_frame(PID, "fr", 1, &rep_path)
                .await
                .unwrap();
            node.collab_store()
                .unwrap()
                .tags()
                .set(
                    format!("in-flight/project/{PID}/fz/1"),
                    iroh_blobs::HashAndFormat::raw(own_hash),
                )
                .await
                .unwrap();
            {
                let conn = db(&fx.ctx).unwrap().conn();
                frames_db::record_own(
                    &conn,
                    &own_row("fm", "published", own_path.to_str().unwrap()),
                )
                .unwrap();
            }
            let project_prefix = format!("project/{PID}/");
            let in_flight_prefix = format!("in-flight/project/{PID}/");
            assert_eq!(collab_tags(&node, &project_prefix).await, 2);
            assert_eq!(collab_tags(&node, &in_flight_prefix).await, 1);

            fx.hub.remove_member(PID, "acc-me");
            let moved = poll_versions_once(&fx.ctx, None).await.unwrap();
            assert!(moved.is_empty(), "a lost project is not a moved one");

            assert_eq!(collab_tags(&node, &project_prefix).await, 0);
            assert_eq!(collab_tags(&node, &in_flight_prefix).await, 0);
            assert!(row(&fx.ctx, "fr").is_none(), "replica rows are deleted");
            let own = row(&fx.ctx, "fm").expect("my own row survives the loss");
            assert_eq!(own.landed_path.as_deref(), own_path.to_str());
            assert!(own_path.exists(), "my own file stays on disk");
            {
                let conn = db(&fx.ctx).unwrap().conn();
                assert!(crate::db::collab::lost_at(&conn, PID).unwrap().is_some());
                assert!(
                    crate::db::collab::list_projects(&conn).unwrap().is_empty(),
                    "a lost project is hidden"
                );
            }
            assert!(crate::api::collab::list_projects(&fx.ctx)
                .unwrap()
                .is_empty());
            let before = request_count(&fx.hub).await;
            poll_versions_once(&fx.ctx, None).await.unwrap();
            assert_eq!(
                requests_since(&fx.hub, before).await.len(),
                1,
                "a lost project is out of the poll"
            );

            fx.hub.add_member(PID, "acc-me", "send_receive", false);
            let moved = poll_versions_once(&fx.ctx, None).await.unwrap();
            assert_eq!(moved, vec![PID.to_string()], "a re-join moves the project");
            {
                let conn = db(&fx.ctx).unwrap().conn();
                assert!(crate::db::collab::lost_at(&conn, PID).unwrap().is_none());
            }
            assert!(
                row(&fx.ctx, "fr").is_some(),
                "the refetch from 0 brings replicas back"
            );
            let own = row(&fx.ctx, "fm").unwrap();
            assert_eq!(own.origin, FrameOrigin::Own);
            assert_eq!(own.landed_path.as_deref(), own_path.to_str());
            assert_eq!(own.blake3, fx.hub.frame(PID, "fm").unwrap().blake3);
            node.shutdown().await;
        }

        /// The hook fires when the thresholds version changes, and not on a
        /// move that changed neither thresholds nor dictionary.
        #[tokio::test]
        async fn threshold_move_calls_the_hook() {
            let fx = fx("send_receive", false, false).await;
            poll_versions_once(&fx.ctx, None).await.unwrap();
            take_gate_moves_seen();

            fx.hub.bump(PID);
            let moved = poll_versions_once(&fx.ctx, None).await.unwrap();
            assert_eq!(moved, vec![PID.to_string()]);
            assert!(
                take_gate_moves_seen().is_empty(),
                "a plain bump is not a thresholds/dictionary move"
            );

            fx.hub.set_thresholds_version(PID, 3);
            poll_versions_once(&fx.ctx, None).await.unwrap();
            assert_eq!(take_gate_moves_seen(), vec![PID.to_string()], "once");
            assert_eq!(project(&fx.ctx).unwrap().thresholds_version, Some(3));
        }

        /// R11: a thresholds change the UI's project refresh absorbed still
        /// fires the hook — once, from that refresh — and the poll that
        /// follows does not fire it again.
        #[tokio::test]
        async fn a_move_absorbed_by_a_ui_refresh_fires_the_hook_once() {
            let fx = fx("send_receive", false, false).await;
            poll_versions_once(&fx.ctx, None).await.unwrap();
            take_gate_moves_seen();

            fx.hub.set_thresholds_version(PID, 2);
            crate::api::collab::refresh_projects(&fx.ctx).await.unwrap();
            assert_eq!(take_gate_moves_seen(), vec![PID.to_string()]);

            let moved = poll_versions_once(&fx.ctx, None).await.unwrap();
            assert_eq!(moved, vec![PID.to_string()], "the version still moved");
            assert!(take_gate_moves_seen().is_empty(), "not a second time");
        }

        /// A dictionary move reaches the cache through the refresh the poll
        /// runs, and fires the hook once.
        #[tokio::test]
        async fn dictionary_move_is_refetched_and_calls_the_hook() {
            use crate::collab::filters::DictionaryEntry;
            let fx = fx("send_receive", false, false).await;
            poll_versions_once(&fx.ctx, None).await.unwrap();
            assert_eq!(project(&fx.ctx).unwrap().dictionary_version, Some(1));
            take_gate_moves_seen();

            fx.hub.set_dictionary(
                PID,
                2,
                vec![DictionaryEntry {
                    canonical: "Sii".into(),
                    aliases: vec!["S2".into()],
                    kind: "narrowband".into(),
                }],
            );
            poll_versions_once(&fx.ctx, None).await.unwrap();
            let p = project(&fx.ctx).unwrap();
            assert_eq!(p.dictionary_version, Some(2));
            let dict: Vec<DictionaryEntry> =
                serde_json::from_str(p.dictionary_json.as_deref().unwrap()).unwrap();
            assert_eq!(dict.len(), 1);
            assert_eq!(dict[0].canonical, "Sii");
            assert_eq!(take_gate_moves_seen(), vec![PID.to_string()]);
        }

        /// R12: a caps change landing between the refresh and the manifest
        /// fetch is still acted on at the next poll — the manifest's own
        /// `projectVersion` (already past the change) is never stored.
        #[tokio::test]
        async fn a_caps_change_between_refresh_and_manifest_is_acted_on_next_poll() {
            let fx = fx("send_receive", false, true).await;
            fx.hub.set_caps(PID, "acc-me", &["data.moderate"]);
            fx.hub.seed_frames(PID, "acc-o", &["fp"], "pending");
            poll_versions_once(&fx.ctx, None).await.unwrap();
            assert!(row(&fx.ctx, "fp").is_some());

            fx.hub.seed_frames(PID, "acc-o", &["fo"], "published");
            fx.hub.before_next("/manifest", |st| {
                let p = st.projects.get_mut(PID).unwrap();
                for m in p.members.iter_mut().filter(|m| m.account_id == "acc-me") {
                    m.gov_caps.clear();
                }
                p.bump();
            });
            poll_versions_once(&fx.ctx, None).await.unwrap();
            assert!(row(&fx.ctx, "fo").is_some());
            assert!(
                row(&fx.ctx, "fp").is_some(),
                "the refresh ran before the revoke"
            );

            let before = request_count(&fx.hub).await;
            let moved = poll_versions_once(&fx.ctx, None).await.unwrap();
            assert_eq!(moved, vec![PID.to_string()], "the revoke is a move");
            assert_eq!(manifest_queries(&fx.hub, before).await, vec![(0, None)]);
            assert!(row(&fx.ctx, "fp").is_none(), "pruned under the new caps");
        }

        /// A moved project whose refresh failed keeps its old `hub_version`:
        /// its manifest sync waits for a poll whose refresh succeeds, so the
        /// thresholds change behind the move is never skipped.
        #[tokio::test]
        async fn a_failed_refresh_defers_the_manifest_sync() {
            let fx = fx("send_receive", false, false).await;
            poll_versions_once(&fx.ctx, None).await.unwrap();
            let synced = project(&fx.ctx).unwrap().hub_version;

            fx.hub.set_thresholds_version(PID, 2);
            fx.hub
                .set_failing(&format!("/projects/{PID}/thresholds"), true);
            let before = request_count(&fx.hub).await;
            poll_versions_once(&fx.ctx, None).await.unwrap();
            assert!(
                manifest_queries(&fx.hub, before).await.is_empty(),
                "no manifest sync after a failed refresh"
            );
            assert_eq!(project(&fx.ctx).unwrap().hub_version, synced);

            fx.hub
                .set_failing(&format!("/projects/{PID}/thresholds"), false);
            clear_poll_backoff_for(&fx.ctx);
            poll_versions_once(&fx.ctx, None).await.unwrap();
            let p = project(&fx.ctx).unwrap();
            assert_eq!(p.thresholds_version, Some(2));
            assert_eq!(
                p.hub_version,
                fx.hub.state.lock().unwrap().projects[PID].version
            );
        }

        /// R13: a persistently failing project backs off to the pass cadence —
        /// the next tick costs no request beyond the version list and logs
        /// nothing above `debug`; the failure itself is logged once.
        #[tokio::test]
        async fn a_failing_project_backs_off_and_logs_once() {
            let fx = fx("send_receive", false, false).await;
            poll_versions_once(&fx.ctx, None).await.unwrap();

            fx.hub.bump(PID);
            fx.hub
                .set_failing(&format!("/projects/{PID}/thresholds"), true);
            let logs = capture_logs();
            assert!(poll_versions_once(&fx.ctx, None).await.unwrap().is_empty());
            let failures = logs.take(&["WARN", "ERROR"]);
            assert_eq!(failures.len(), 1, "logged once: {failures:?}");

            let before = request_count(&fx.hub).await;
            assert!(poll_versions_once(&fx.ctx, None).await.unwrap().is_empty());
            assert_eq!(
                requests_since(&fx.hub, before).await,
                vec![("GET".to_string(), "/api/v1/me/project-versions".to_string())],
                "a backed-off project costs no request"
            );
            assert!(logs.take(&["WARN", "ERROR", "INFO"]).is_empty());

            fx.hub
                .set_failing(&format!("/projects/{PID}/thresholds"), false);
            clear_poll_backoff_for(&fx.ctx);
            assert_eq!(
                poll_versions_once(&fx.ctx, None).await.unwrap(),
                vec![PID.to_string()],
                "retried once the back-off is over"
            );
        }

        /// Signed out: the poll is a silent `Ok(vec![])`.
        #[tokio::test]
        async fn poll_is_a_no_op_when_signed_out() {
            let (_tmp, ctx) = test_ctx();
            assert!(poll_versions_once(&ctx, None).await.unwrap().is_empty());
        }

        /// Only a poll that moved something kicks the pass.
        #[test]
        fn poll_kicks_the_pass_only_when_a_version_moved() {
            assert!(!kick_if_versions_moved(&[]));
            assert!(kick_if_versions_moved(&["p1".to_string()]));
        }

        /// R15: the version poll ticks on its own loop — nothing gates it.
        #[tokio::test]
        async fn the_version_poll_ticks_on_its_own_loop() {
            let polls = Arc::new(AtomicUsize::new(0));
            let task = tokio::spawn({
                let polls = Arc::clone(&polls);
                tick_loop(Duration::ZERO, Duration::from_millis(10), move || {
                    let polls = Arc::clone(&polls);
                    async move {
                        polls.fetch_add(1, Ordering::SeqCst);
                    }
                })
            });
            wait_until(|| polls.load(Ordering::SeqCst) >= 3, Duration::from_secs(5)).await;
            task.abort();
        }

        /// A kick stored before a timer pass (a version move during the
        /// startup grace) is covered by that pass: no second pass right
        /// after. A later kick still wakes the loop.
        #[tokio::test]
        async fn a_kick_before_a_timer_pass_buys_no_second_pass() {
            let passes = Arc::new(AtomicUsize::new(0));
            let kick = Arc::new(tokio::sync::Notify::new());
            kick.notify_one();
            let task = tokio::spawn({
                let passes = Arc::clone(&passes);
                let kick = Arc::clone(&kick);
                async move {
                    auto_sync_loop_inner(
                        Duration::from_millis(20),
                        Duration::from_secs(3600),
                        &kick,
                        move || {
                            let passes = Arc::clone(&passes);
                            async move {
                                passes.fetch_add(1, Ordering::SeqCst);
                            }
                        },
                    )
                    .await
                }
            });
            wait_until(
                || passes.load(Ordering::SeqCst) >= 1,
                Duration::from_secs(5),
            )
            .await;
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert_eq!(passes.load(Ordering::SeqCst), 1, "no double pass");
            kick.notify_one();
            wait_until(
                || passes.load(Ordering::SeqCst) >= 2,
                Duration::from_secs(5),
            )
            .await;
            task.abort();
        }
    }

    #[cfg(all(feature = "render", feature = "solver"))]
    mod manifest {
        use super::v3_fx::*;
        use super::*;

        /// Page size 2, five frames sharing ONE manifest version: one
        /// `sync_manifest` follows `next` (the `after` tiebreaker) to the end.
        /// A sync outside the poll vouches for no version (R12).
        #[tokio::test]
        async fn paging_follows_next() {
            let fx = fx("send_receive", false, false).await;
            fx.hub.set_page_size(2);
            fx.hub
                .seed_frames(PID, "acc-o", &["f1", "f2", "f3", "f4", "f5"], "published");
            crate::api::collab::refresh_projects(&fx.ctx).await.unwrap();

            let before = request_count(&fx.hub).await;
            sync_manifest(&fx.ctx, PID, None, None).await.unwrap();
            assert_eq!(rows(&fx.ctx).len(), 5, "all five stored");
            let mv = fx.hub.frame(PID, "f1").unwrap().manifest_version;
            assert_eq!(
                manifest_queries(&fx.hub, before).await,
                vec![
                    (0, None),
                    (mv, Some("f2".to_string())),
                    (mv, Some("f4".to_string())),
                ]
            );
            let p = project(&fx.ctx).unwrap();
            assert_eq!(p.manifest_cursor, mv);
            assert_eq!(p.hub_version, 0, "no vouched version, no hub_version");
        }

        /// Losing `data.moderate` re-fetches from 0 and prunes the pending row
        /// of another member I can no longer see; own rows survive.
        #[tokio::test]
        async fn caps_change_refetches_from_zero_and_prunes() {
            let fx = fx("send_receive", false, true).await;
            fx.hub.set_caps(PID, "acc-me", &["data.moderate"]);
            fx.hub.seed_frames(PID, "acc-o", &["fp"], "pending");
            fx.hub.seed_frames(PID, "acc-o", &["fo"], "published");
            fx.hub.seed_frames(PID, "acc-me", &["fm"], "published");

            poll_versions_once(&fx.ctx, None).await.unwrap();
            assert!(
                row(&fx.ctx, "fp").is_some(),
                "a moderator sees the pending row"
            );
            assert_eq!(
                project(&fx.ctx).unwrap().synced_caps_json,
                r#"["data.moderate"]"#
            );
            // An own row the hub does not list (yet) must survive the prune too.
            {
                let conn = db(&fx.ctx).unwrap().conn();
                frames_db::record_own(&conn, &own_row("fx", "unknown", "/nowhere/fx.fits"))
                    .unwrap();
            }

            fx.hub.set_caps(PID, "acc-me", &[]);
            let before = request_count(&fx.hub).await;
            let moved = poll_versions_once(&fx.ctx, None).await.unwrap();
            assert_eq!(moved, vec![PID.to_string()]);
            assert_eq!(
                manifest_queries(&fx.hub, before).await,
                vec![(0, None)],
                "a caps change refetches from 0"
            );
            assert!(row(&fx.ctx, "fp").is_none(), "the pending row is pruned");
            assert!(row(&fx.ctx, "fo").is_some());
            assert_eq!(row(&fx.ctx, "fm").unwrap().origin, FrameOrigin::Own);
            assert!(row(&fx.ctx, "fx").is_some(), "own rows are never pruned");
            assert_eq!(project(&fx.ctx).unwrap().synced_caps_json, "[]");
        }

        /// The refresh stores the hub's pending count and this member's caps.
        #[tokio::test]
        async fn refresh_stores_pending_frames_and_caps() {
            let fx = fx("send_receive", false, true).await;
            fx.hub.set_caps(PID, "acc-me", &["data.moderate"]);
            fx.hub.seed_frames(PID, "acc-o", &["p1", "p2"], "pending");
            crate::api::collab::refresh_projects(&fx.ctx).await.unwrap();
            let p = project(&fx.ctx).unwrap();
            assert_eq!(p.pending_frames, 2);
            assert_eq!(p.gov_caps_json, r#"["data.moderate"]"#);
            assert_eq!(p.synced_caps_json, "[]", "only a manifest sync writes it");
        }

        /// A coordinator carries a `"coordinator"` caps element, so a
        /// coordinator flip is a caps change too.
        #[tokio::test]
        async fn coordinator_is_part_of_the_caps() {
            let fx = fx("send_receive", true, false).await;
            crate::api::collab::refresh_projects(&fx.ctx).await.unwrap();
            assert_eq!(
                project(&fx.ctx).unwrap().gov_caps_json,
                r#"["coordinator"]"#
            );
        }

        /// Every `FramesChangeKind` driven through the fake, each emitted once
        /// with its count.
        #[tokio::test]
        async fn change_kinds_are_classified() {
            let fx = fx("send_receive", false, true).await;
            fx.hub.set_caps(PID, "acc-me", &["data.moderate"]);
            fx.hub.seed_frames(PID, "acc-me", &["a", "b"], "pending");
            fx.hub.seed_frames(PID, "acc-o", &["c", "d"], "published");
            crate::api::collab::refresh_projects(&fx.ctx).await.unwrap();

            let first = RecordingEmitter::default();
            sync_manifest(&fx.ctx, PID, Some(&first), None)
                .await
                .unwrap();
            assert_eq!(first.frame_changes(), vec![("newFrames".to_string(), 2)]);

            fx.hub.seed_frames(PID, "acc-o", &["e"], "published");
            fx.hub.seed_frames(PID, "acc-o", &["f"], "pending");
            fx.hub
                .update_frame(PID, "a", |f| f.state = "published".into());
            fx.hub.update_frame(PID, "b", |f| {
                f.state = "rejected".into();
                f.reject_reason = Some("soft".into());
            });
            fx.hub.set_accepted(PID, "d", false, Some("trailing"));
            fx.hub.update_frame(PID, "c", |f| {
                f.content_version = 2;
                f.blake3 = "c".repeat(64);
            });

            let rec = RecordingEmitter::default();
            let changes = sync_manifest(&fx.ctx, PID, Some(&rec), None).await.unwrap();
            let expected: Vec<(String, u64)> = [
                "approved",
                "excluded",
                "newFrames",
                "newVersions",
                "pendingFrames",
                "rejected",
            ]
            .iter()
            .map(|k| (k.to_string(), 1))
            .collect();
            assert_eq!(rec.frame_changes(), expected, "one event per kind");
            assert_eq!(changes.len(), 6);
            assert!(changes.iter().all(|c| c.count == 1 && c.project_id == PID));
        }

        /// Syncs of one project are serialized: two running in parallel (the
        /// poll's and the pass's, on two worker threads) apply a delta once,
        /// so its events are emitted once.
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn concurrent_syncs_emit_a_delta_once() {
            let Fx {
                tmp: _tmp,
                ctx,
                hub,
            } = fx("send_receive", false, false).await;
            crate::api::collab::refresh_projects(&ctx).await.unwrap();
            let uuids: Vec<String> = (0..300).map(|i| format!("f{i:03}")).collect();
            let refs: Vec<&str> = uuids.iter().map(String::as_str).collect();
            hub.seed_frames(PID, "acc-o", &refs, "published");

            let ctx = Arc::new(ctx);
            let rec = Arc::new(RecordingEmitter::default());
            let tasks: Vec<_> = (0..4)
                .map(|_| {
                    let ctx = Arc::clone(&ctx);
                    let rec = Arc::clone(&rec);
                    tokio::spawn(async move {
                        let em: &dyn ProgressEmitter = &*rec;
                        sync_manifest(&ctx, PID, Some(em), None).await.unwrap();
                    })
                })
                .collect();
            for t in tasks {
                t.await.unwrap();
            }
            assert_eq!(rec.frame_changes(), vec![("newFrames".to_string(), 300)]);
        }

        /// R8: an adopted own row (state `unknown`, LOCAL hashes at v1,
        /// `accepted` false) takes the hub's columns from the manifest while
        /// its local columns stay.
        #[tokio::test]
        async fn manifest_sync_corrects_an_adopted_own_row() {
            let fx = fx("send_receive", false, false).await;
            fx.hub.seed_frames(PID, "acc-me", &["fa"], "published");
            fx.hub.update_frame(PID, "fa", |f| {
                f.content_version = 2;
                f.byte_size = 4242;
            });
            crate::api::collab::refresh_projects(&fx.ctx).await.unwrap();
            {
                let conn = db(&fx.ctx).unwrap().conn();
                let mut adopted = own_row("fa", "unknown", "/landed/fa.fits");
                adopted.accepted = false;
                frames_db::record_own(&conn, &adopted).unwrap();
            }

            sync_manifest(&fx.ctx, PID, None, None).await.unwrap();

            let hub = fx.hub.frame(PID, "fa").unwrap();
            let r = row(&fx.ctx, "fa").unwrap();
            assert_eq!(r.origin, FrameOrigin::Own);
            assert_eq!(r.state, "published");
            assert!(r.accepted, "accepted is hub state");
            assert_eq!(r.content_version, 2);
            assert_eq!(r.blake3, hub.blake3);
            assert_eq!(r.xxh3, hub.xxh3);
            assert_eq!(r.byte_size, 4242);
            assert_eq!(r.manifest_version, hub.manifest_version);
            assert_eq!(r.landed_path.as_deref(), Some("/landed/fa.fits"));
            assert_eq!(r.source_frame_id, Some(42));
            assert_eq!(r.recipe_hash.as_deref(), Some("recipe"));
            assert!(r.on_disk, "an own row keeps on_disk across a version move");
        }

        /// The pure classifier, one row at a time.
        #[test]
        fn classify_frame_change_rules() {
            use FramesChangeKind as K;
            let view = |own: bool, state: &str, accepted: bool, cv: i32| FrameViewWire {
                frame_uuid: "u".into(),
                publisher_account_id: "a".into(),
                publisher_display_name: "A".into(),
                own,
                file_name: "u.fits".into(),
                content_version: cv,
                blake3: "0".repeat(64),
                byte_size: 1,
                xxh3: "0".repeat(16),
                filter_raw: "L".into(),
                filter_canonical: "L".into(),
                channel: "mono".into(),
                exptime_sec: 1.0,
                date_obs: None,
                meta: serde_json::json!({}),
                gate_version: 0,
                accepted,
                accepted_reason: None,
                state: state.into(),
                reject_reason: None,
                manifest_version: 1,
                created_at: String::new(),
                holder_count: 0,
            };
            let prev = |origin: FrameOrigin, state: &str, accepted: bool, cv: i32| {
                let mut r = own_row("u", state, "/x");
                r.origin = origin;
                r.accepted = accepted;
                r.content_version = cv;
                r
            };
            let rep = FrameOrigin::Replica;
            let own = FrameOrigin::Own;

            assert_eq!(
                classify_frame_change(None, &view(false, "published", true, 1)),
                vec![K::NewFrames]
            );
            assert_eq!(
                classify_frame_change(None, &view(false, "pending", true, 1)),
                vec![K::PendingFrames]
            );
            assert!(classify_frame_change(None, &view(true, "published", true, 1)).is_empty());
            assert_eq!(
                classify_frame_change(
                    Some(&prev(rep, "pending", true, 1)),
                    &view(false, "published", true, 1)
                ),
                vec![K::NewFrames],
                "a pending row another moderator approved is new to me"
            );
            assert_eq!(
                classify_frame_change(
                    Some(&prev(own, "pending", true, 1)),
                    &view(true, "published", true, 1)
                ),
                vec![K::Approved]
            );
            assert_eq!(
                classify_frame_change(
                    Some(&prev(own, "pending", true, 1)),
                    &view(true, "rejected", true, 1)
                ),
                vec![K::Rejected]
            );
            assert_eq!(
                classify_frame_change(
                    Some(&prev(rep, "published", true, 1)),
                    &view(false, "published", false, 1)
                ),
                vec![K::Excluded]
            );
            assert_eq!(
                classify_frame_change(
                    Some(&prev(rep, "published", true, 1)),
                    &view(false, "published", true, 2)
                ),
                vec![K::NewVersions]
            );
            assert!(
                classify_frame_change(
                    Some(&prev(own, "published", true, 1)),
                    &view(true, "published", true, 2)
                )
                .is_empty(),
                "my own new version is not news to me"
            );
            assert!(
                classify_frame_change(
                    Some(&prev(rep, "published", true, 1)),
                    &view(false, "published", true, 1)
                )
                .is_empty(),
                "an unchanged row is no change"
            );
        }
    }

    // ── Task 9 (wave 2): the need set, pure ──────────────────────────────────

    mod need {
        use super::*;

        pub(super) fn replica(uuid: &str, holders: i64, created: &str, size: i64) -> LocalFrameRow {
            LocalFrameRow {
                project_id: "p1".into(),
                frame_uuid: uuid.into(),
                content_version: 1,
                origin: FrameOrigin::Replica,
                publisher_account_id: "acc-o".into(),
                publisher_display: "Other".into(),
                file_name: format!("{uuid}.fits"),
                filter_canonical: "L".into(),
                state: "published".into(),
                accepted: true,
                byte_size: size,
                xxh3: "0".repeat(16),
                blake3: "0".repeat(64),
                holder_count: holders,
                manifest_version: 1,
                manifest_json: serde_json::json!({ "createdAt": created, "meta": {} }).to_string(),
                landed_path: None,
                size_mtime_seen: None,
                on_disk: false,
                locally_declined: false,
                awaiting_gc: false,
                source_frame_id: None,
                recipe_hash: None,
                last_error: None,
                updated_at: String::new(),
            }
        }

        fn uuids(v: &[LocalFrameRow]) -> Vec<&str> {
            v.iter().map(|r| r.frame_uuid.as_str()).collect()
        }

        fn all() -> ReplicationPolicy {
            ReplicationPolicy::default()
        }

        /// Spec §5.3: only a published, accepted peer frame that is not on
        /// disk, not declined and not waiting for GC is needed.
        #[test]
        fn need_excludes_own_declined_unaccepted_pending_and_awaiting_gc() {
            let ok = replica("ok", 1, "t", 10);
            let mut own = replica("own", 1, "t", 10);
            own.origin = FrameOrigin::Own;
            let mut declined = replica("declined", 1, "t", 10);
            declined.locally_declined = true;
            let mut unaccepted = replica("unaccepted", 1, "t", 10);
            unaccepted.accepted = false;
            let mut pending = replica("pending", 1, "t", 10);
            pending.state = "pending".into();
            let mut rejected = replica("rejected", 1, "t", 10);
            rejected.state = "rejected".into();
            let mut awaiting = replica("awaiting", 1, "t", 10);
            awaiting.awaiting_gc = true;
            let mut held = replica("held", 1, "t", 10);
            held.on_disk = true;
            let rows = vec![
                ok, own, declined, unaccepted, pending, rejected, awaiting, held,
            ];
            assert_eq!(
                uuids(&frame_need(&rows, &all(), true, true, false)),
                vec!["ok"]
            );
        }

        /// Rarest first (`holderCount`), then oldest (`createdAt`).
        #[test]
        fn need_is_rarest_first_then_oldest() {
            let rows = vec![
                replica("a", 3, "2026-09-01T00:00:01Z", 10),
                replica("b", 1, "2026-09-01T00:00:03Z", 10),
                replica("c", 1, "2026-09-01T00:00:02Z", 10),
                replica("d", 2, "2026-09-01T00:00:00Z", 10),
            ];
            assert_eq!(
                uuids(&frame_need(&rows, &all(), true, true, false)),
                vec!["c", "b", "d", "a"]
            );
        }

        /// The budget counts the replicas already on disk (own frames are not
        /// replicas) and stops at the first frame that would cross it.
        #[test]
        fn byte_budget_counts_already_held_bytes() {
            let mut held = replica("held", 1, "t0", 600);
            held.on_disk = true;
            let mut own = replica("own", 1, "t0", 5000);
            own.origin = FrameOrigin::Own;
            own.on_disk = true;
            let rows = vec![
                held,
                own,
                replica("a", 1, "t1", 300),
                replica("b", 1, "t2", 200),
                replica("c", 1, "t3", 10),
            ];
            let policy = ReplicationPolicy {
                byte_budget: Some(1000),
                ..all()
            };
            assert_eq!(
                uuids(&frame_need(&rows, &policy, true, true, false)),
                vec!["a"],
                "600 held + 300 fits, + 200 would not — and the walk stops there"
            );
            let roomy = ReplicationPolicy {
                byte_budget: Some(1100),
                ..all()
            };
            assert_eq!(
                uuids(&frame_need(&rows, &roomy, true, true, false)),
                vec!["a", "b"]
            );
        }

        /// Filters, publishers, FWHM and star bounds each narrow the set; a
        /// frame without the measurement never matches a set bound.
        #[test]
        fn policy_filters_by_canonical_filter_publisher_fwhm_stars() {
            let with_meta = |uuid: &str, filter: &str, publisher: &str, meta: serde_json::Value| {
                let mut r = replica(uuid, 1, uuid, 10);
                r.filter_canonical = filter.into();
                r.publisher_account_id = publisher.into();
                r.manifest_json =
                    serde_json::json!({ "createdAt": uuid, "meta": meta }).to_string();
                r
            };
            let rows = vec![
                with_meta(
                    "1",
                    "L",
                    "acc-o",
                    serde_json::json!({ "fwhmArcsec": 2.0, "starsDetected": 100 }),
                ),
                with_meta(
                    "2",
                    "Ha",
                    "acc-o",
                    serde_json::json!({ "fwhmArcsec": 4.0, "starsDetected": 10 }),
                ),
                with_meta("3", "L", "acc-x", serde_json::json!({})),
            ];
            let need = |p: ReplicationPolicy| {
                uuids(&frame_need(&rows, &p, true, true, false))
                    .into_iter()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            };
            assert_eq!(need(all()), vec!["1", "2", "3"]);
            assert_eq!(
                need(ReplicationPolicy {
                    filters: vec!["Ha".into()],
                    ..all()
                }),
                vec!["2"]
            );
            assert_eq!(
                need(ReplicationPolicy {
                    publishers: vec!["acc-x".into()],
                    ..all()
                }),
                vec!["3"]
            );
            assert_eq!(
                need(ReplicationPolicy {
                    max_fwhm_arcsec: Some(3.0),
                    ..all()
                }),
                vec!["1"],
                "4.0 is over the bound and a missing FWHM never matches"
            );
            assert_eq!(
                need(ReplicationPolicy {
                    min_stars: Some(50),
                    ..all()
                }),
                vec!["1"]
            );
        }

        /// A paused project, a role that may not replicate, and an
        /// auto-replicate toggle that is off all need nothing.
        #[test]
        fn paused_needs_nothing() {
            let rows = vec![replica("a", 1, "t", 10)];
            assert!(frame_need(&rows, &all(), true, true, true).is_empty());
            assert!(frame_need(&rows, &all(), false, true, false).is_empty());
            assert!(frame_need(&rows, &all(), true, false, false).is_empty());
            assert_eq!(frame_need(&rows, &all(), true, true, false).len(), 1);
        }
    }

    // ── Task 9 (wave 2): the replication pass against the fake hub ───────────

    #[cfg(all(feature = "render", feature = "solver"))]
    mod replication {
        use super::v3_fx::*;
        use super::*;
        use crate::sharing::iroh::node::{BlobHealth, SharedIrohNode};

        /// The v3 fixture with the context in an `Arc` (the loss command
        /// takes one), the project refreshed into the cache and the
        /// Collaboration root `<tmp>/Collab` designated. No node is bound.
        pub(super) struct RFx {
            pub tmp: tempfile::TempDir,
            pub ctx: Arc<ServiceContext>,
            pub hub: FakeHub,
            pub collab: PathBuf,
        }

        pub(super) async fn rfx(role: &str) -> RFx {
            let Fx { tmp, ctx, hub } = fx(role, false, false).await;
            crate::api::collab::refresh_projects(&ctx).await.unwrap();
            let requested = tmp.path().join("Collab");
            std::fs::create_dir_all(&requested).unwrap();
            let collab = PathBuf::from(
                crate::api::scan_roots::set_collaboration_dir(
                    &ctx,
                    requested.to_string_lossy().to_string(),
                    &crate::api::PathPolicy::AllowAll,
                )
                .await
                .unwrap(),
            );
            RFx {
                tmp,
                ctx: Arc::new(ctx),
                hub,
                collab,
            }
        }

        /// A recording fetch seam: `(project, frame uuids)` per call; errs
        /// for the projects in `fail`.
        #[derive(Default)]
        pub(super) struct FetchRecorder {
            pub calls: std::sync::Mutex<Vec<(String, Vec<String>)>>,
            pub fail: std::sync::Mutex<HashSet<String>>,
        }

        impl FetchRecorder {
            pub(super) fn projects(&self) -> Vec<String> {
                self.calls
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(p, _)| p.clone())
                    .collect()
            }
        }

        pub(super) async fn pass_with(
            ctx: &ServiceContext,
            rec: &Arc<FetchRecorder>,
            kind: PassKind,
            scope: Option<&str>,
            emitter: Option<&dyn ProgressEmitter>,
        ) -> AutoSyncPassOutcome {
            let rec = Arc::clone(rec);
            run_auto_sync_pass(ctx, kind, scope, emitter, move |project_id, need| {
                let rec = Arc::clone(&rec);
                async move {
                    let uuids: Vec<String> = need.iter().map(|r| r.frame_uuid.clone()).collect();
                    let n = uuids.len();
                    rec.calls.lock().unwrap().push((project_id.clone(), uuids));
                    if rec.fail.lock().unwrap().contains(&project_id) {
                        return Err(ApiError::Internal(format!("fetch {project_id} exploded")));
                    }
                    Ok(FetchOutcome {
                        landed: n,
                        ..Default::default()
                    })
                }
            })
            .await
        }

        /// Write `bytes` under the Collaboration root and record a landed row
        /// for it (`origin`), as a landing or a publish would have.
        pub(super) fn land_file(r: &RFx, uuid: &str, origin: FrameOrigin, bytes: &[u8]) -> PathBuf {
            let dir = r.collab.join("m31").join("other");
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(format!("{uuid}.fits"));
            std::fs::write(&path, bytes).unwrap();
            let meta = std::fs::metadata(&path).unwrap();
            let mut row = own_row(uuid, "published", &path.to_string_lossy());
            row.origin = origin;
            if origin == FrameOrigin::Replica {
                row.publisher_account_id = "acc-o".into();
                row.publisher_display = "Other".into();
                row.source_frame_id = None;
                row.recipe_hash = None;
            }
            row.byte_size = bytes.len() as i64;
            row.xxh3 = hash_bytes(bytes);
            row.blake3 = blake3::hash(bytes).to_hex().to_string();
            row.size_mtime_seen = Some(size_mtime_from(&meta));
            frames_db::record_own(&db(&r.ctx).unwrap().conn(), &row).unwrap();
            path
        }

        /// Move a file's mtime `secs` into the future.
        pub(super) fn bump_mtime(path: &Path, secs: u64) {
            let t =
                std::fs::metadata(path).unwrap().modified().unwrap() + Duration::from_secs(secs);
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(t)
                .unwrap();
        }

        /// A relay-disabled node installed on `r.ctx` with the Collaboration
        /// root mounted.
        pub(super) async fn bind_receiver(r: &RFx) -> Arc<SharedIrohNode> {
            let dirs = crate::api::sync::sync_dirs(&r.ctx).unwrap();
            std::fs::create_dir_all(&dirs.identity_dir).unwrap();
            std::fs::create_dir_all(&dirs.working_dir).unwrap();
            let node = SharedIrohNode::bind_with(
                &dirs.identity_dir,
                &dirs.working_dir,
                iroh::RelayMode::Disabled,
                crate::sharing::iroh::node::NodeOptions::default(),
            )
            .await
            .expect("bind relay-disabled node");
            *r.ctx.iroh_node.lock().await = Some(Arc::clone(&node));
            node.set_collab_root(Some(&r.collab)).await.unwrap();
            node
        }

        /// A bare relay-disabled node with its own Collaboration root
        /// mounted — another member's app, reduced to its store.
        pub(super) async fn peer_node(dir: &Path) -> Arc<SharedIrohNode> {
            let node = SharedIrohNode::bind(&dir.join("sync"), iroh::RelayMode::Disabled)
                .await
                .unwrap();
            let root = dir.join("Collab");
            std::fs::create_dir_all(&root).unwrap();
            node.set_collab_root(Some(&root)).await.unwrap();
            node
        }

        /// Relay-disabled nodes have no discovery: exchange addresses.
        pub(super) async fn pair(a: &Arc<SharedIrohNode>, b: &Arc<SharedIrohNode>) {
            for n in [a, b] {
                n.handle(Role::Out).start().await.unwrap();
            }
            a.add_peer(b.endpoint_addr());
            b.add_peer(a.endpoint_addr());
        }

        /// `acc-o` publishes `bytes` as `uuid` / `name`: seeded by reference
        /// on `publisher`, listed by the hub with `publisher` as its holder.
        pub(super) async fn publish_on(
            r: &RFx,
            publisher: &Arc<SharedIrohNode>,
            uuid: &str,
            name: &str,
            bytes: &[u8],
        ) -> iroh_blobs::Hash {
            let dir = r.tmp.path().join(format!("pub-{uuid}"));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            let hash = publisher
                .seed_project_frame(PID, uuid, 1, &path)
                .await
                .expect("the publisher seeds its frame");
            set_publisher_key(r, publisher);
            r.hub.seed_frames(PID, "acc-o", &[uuid], "published");
            let (name, xxh3, size) = (name.to_string(), hash_bytes(bytes), bytes.len() as i64);
            r.hub.update_frame(PID, uuid, move |f| {
                f.file_name = name;
                f.blake3 = hash.to_hex().to_string();
                f.xxh3 = xxh3;
                f.byte_size = size;
            });
            hash
        }

        /// `acc-o`'s device is `publisher`.
        pub(super) fn set_publisher_key(r: &RFx, publisher: &Arc<SharedIrohNode>) {
            r.hub.add_account(
                "tok-o",
                "acc-o",
                "Other",
                &B64.encode(publisher.node_id()),
                None,
            );
        }

        pub(super) fn dir_bytes(dir: &Path) -> u64 {
            let mut total = 0;
            if let Ok(entries) = std::fs::read_dir(dir) {
                for e in entries.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        total += dir_bytes(&p);
                    } else if let Ok(m) = p.metadata() {
                        total += m.len();
                    }
                }
            }
            total
        }

        pub(super) fn pattern(seed: usize, len: usize) -> Vec<u8> {
            (0..len)
                .map(|j| ((j * 31 + seed * 97) % 251) as u8)
                .collect()
        }

        pub(super) async fn real_pass(r: &RFx, kind: PassKind) -> AutoSyncPassOutcome {
            let sync = crate::sync::SyncRuntime::new();
            tokio::time::timeout(
                Duration::from_secs(120),
                replication_pass(&r.ctx, &sync, kind, None, None),
            )
            .await
            .expect("a replication pass must not take two minutes")
        }

        /// What the worker does on a timer tick: maintenance, then a fetch
        /// pass.
        pub(super) async fn real_cycle(r: &RFx) -> AutoSyncPassOutcome {
            run_maintenance(&r.ctx, None, None).await;
            real_pass(r, PassKind::Fetch).await
        }

        pub(super) async fn sync_rows(r: &RFx) {
            sync_manifest(&r.ctx, PID, None, None).await.unwrap();
        }

        // ── disk truth ───────────────────────────────────────────────────────

        /// A deleted replica is missing: `on_disk` drops, its seed tags go,
        /// and its store entry (dead now) makes it wait for GC (P20).
        #[tokio::test]
        async fn deleted_replica_is_missing_and_unseeded() {
            let r = rfx("send_receive").await;
            let node = bind_receiver(&r).await;
            let path = land_file(&r, "f1", FrameOrigin::Replica, &pattern(1, 64 * 1024));
            node.seed_project_frame(PID, "f1", 1, &path).await.unwrap();
            assert_eq!(collab_tags(&node, "project/p1/").await, 1);

            std::fs::remove_file(&path).unwrap();
            let truth = disk_truth(&r.ctx, PID).await.unwrap();

            assert_eq!(truth.missing_replicas, vec!["f1".to_string()]);
            assert!(truth.present.is_empty());
            assert_eq!(truth.held_replicas, 1);
            assert_eq!(truth.missing_bytes, 64 * 1024);
            let row = row(&r.ctx, "f1").unwrap();
            assert!(!row.on_disk);
            assert!(
                row.awaiting_gc,
                "the entry still reads the deleted path: fetch only after GC"
            );
            assert_eq!(collab_tags(&node, "project/p1/").await, 0, "unseeded");
            node.shutdown().await;
        }

        /// Same size, new mtime, different bytes: an edit counts as missing.
        #[tokio::test]
        async fn edited_file_counts_as_missing() {
            let r = rfx("send_receive").await;
            let path = land_file(&r, "f1", FrameOrigin::Replica, &pattern(1, 4096));
            std::fs::write(&path, pattern(2, 4096)).unwrap();
            bump_mtime(&path, 10);

            let truth = disk_truth(&r.ctx, PID).await.unwrap();
            assert_eq!(truth.rehashed, 1);
            assert_eq!(truth.missing_replicas, vec!["f1".to_string()]);
            let row = row(&r.ctx, "f1").unwrap();
            assert!(!row.on_disk);
            assert!(row.awaiting_gc, "no store to ask: wait, never fetch blind");
        }

        /// A touched file with the same bytes stays held; its new
        /// `size:mtime` is recorded so the next walk does not re-hash it.
        #[tokio::test]
        async fn touched_but_identical_file_is_kept_and_mtime_updated() {
            let r = rfx("send_receive").await;
            let path = land_file(&r, "f1", FrameOrigin::Replica, &pattern(1, 4096));
            let before = row(&r.ctx, "f1").unwrap().size_mtime_seen;
            bump_mtime(&path, 10);

            let truth = disk_truth(&r.ctx, PID).await.unwrap();
            assert_eq!(truth.rehashed, 1);
            assert_eq!(truth.present, vec![("f1".to_string(), 1)]);
            assert!(truth.missing_replicas.is_empty());
            let row = row(&r.ctx, "f1").unwrap();
            assert!(row.on_disk);
            assert_ne!(row.size_mtime_seen, before);
            assert_eq!(
                row.size_mtime_seen.as_deref(),
                Some(size_mtime_from(&std::fs::metadata(&path).unwrap()).as_str())
            );
            let again = disk_truth(&r.ctx, PID).await.unwrap();
            assert_eq!(again.rehashed, 0, "the new size:mtime is trusted");
        }

        /// My own deleted file is missing too — and never needed: own frames
        /// are never fetched back.
        #[tokio::test]
        async fn deleted_own_file_is_missing_own_and_never_needed() {
            let r = rfx("send_receive").await;
            let path = land_file(&r, "f1", FrameOrigin::Own, &pattern(1, 4096));
            std::fs::remove_file(&path).unwrap();

            let truth = disk_truth(&r.ctx, PID).await.unwrap();
            assert_eq!(truth.missing_own, vec!["f1".to_string()]);
            assert!(truth.missing_replicas.is_empty());
            let rows = rows(&r.ctx);
            assert!(!rows[0].on_disk);
            assert!(!rows[0].awaiting_gc);
            assert!(frame_need(&rows, &ReplicationPolicy::default(), true, true, false).is_empty());
        }

        // ── the loss guard ───────────────────────────────────────────────────

        fn paused(r: &RFx) -> bool {
            project(&r.ctx).unwrap().replication_paused
        }

        fn paused_events(em: &RecordingEmitter) -> Vec<serde_json::Value> {
            em.events
                .lock()
                .unwrap()
                .iter()
                .filter(|(n, _)| n == COLLAB_REPLICATION_PAUSED_EVENT)
                .map(|(_, p)| p.clone())
                .collect()
        }

        /// Land `n` replicas and delete the first `lose`.
        fn land_and_lose(r: &RFx, n: usize, lose: usize, size: usize) {
            for i in 0..n {
                let path = land_file(
                    r,
                    &format!("f{i:02}"),
                    FrameOrigin::Replica,
                    &pattern(i, size),
                );
                if i < lose {
                    std::fs::remove_file(&path).unwrap();
                }
            }
        }

        /// One deleted replica never trips the guard, whatever the fraction.
        #[tokio::test]
        async fn one_deleted_replica_does_not_trip_the_guard() {
            let r = rfx("send_receive").await;
            land_and_lose(&r, 3, 1, 256);
            let em = RecordingEmitter::default();
            run_maintenance(&r.ctx, None, Some(&em)).await;
            assert!(!paused(&r));
            assert!(paused_events(&em).is_empty());
            assert!(!row(&r.ctx, "f00").unwrap().on_disk, "still missing");
        }

        /// 2 of 18 held replicas (11 %) gone: paused, one event, one warn.
        #[tokio::test]
        async fn eleven_percent_missing_trips_and_pauses() {
            let r = rfx("send_receive").await;
            land_and_lose(&r, 18, 2, 256);
            let em = RecordingEmitter::default();
            let logs = capture_logs();
            run_maintenance(&r.ctx, None, Some(&em)).await;
            assert!(paused(&r));
            let events = paused_events(&em);
            assert_eq!(events.len(), 1);
            assert_eq!(events[0]["projectId"], PID);
            assert_eq!(events[0]["missing"], 2);
            assert_eq!(events[0]["missingBytes"], 512);
            assert!(logs
                .take(&["WARN"])
                .iter()
                .any(|m| m.contains("replication paused by the loss guard")));

            // Already paused: the guard does not fire again.
            let em2 = RecordingEmitter::default();
            std::fs::remove_file(row(&r.ctx, "f05").unwrap().landed_path.unwrap()).unwrap();
            std::fs::remove_file(row(&r.ctx, "f06").unwrap().landed_path.unwrap()).unwrap();
            run_maintenance(&r.ctx, None, Some(&em2)).await;
            assert!(paused_events(&em2).is_empty());
        }

        /// The byte threshold trips even when the fraction does not (2 of 30
        /// is 6.7 %).
        #[tokio::test]
        async fn bytes_threshold_trips_below_the_fraction() {
            let r = rfx("send_receive").await;
            {
                let conn = db(&r.ctx).unwrap().conn();
                r.ctx
                    .settings
                    .persist_setting(&conn, crate::settings::keys::COLLAB_LOSS_GUARD_BYTES, "100")
                    .unwrap();
            }
            land_and_lose(&r, 30, 2, 64);
            run_maintenance(&r.ctx, None, None).await;
            assert!(paused(&r), "128 bytes missing > 100");
        }

        /// "Stop holding" declines exactly the replicas whose file is really
        /// lost, never a row waiting for a new version or an intact file
        /// (R22), and unpauses.
        #[tokio::test]
        async fn stop_holding_declines_the_missing_and_unpauses() {
            let r = rfx("send_receive").await;
            land_and_lose(&r, 18, 2, 256);
            run_maintenance(&r.ctx, None, None).await;
            assert!(paused(&r));

            // R22: two not-on-disk rows that are NOT losses — one waiting for
            // a new version with its old file intact, one whose file is
            // intact (a parked or not-yet-re-admitted frame).
            land_file(&r, "vp", FrameOrigin::Replica, &pattern(40, 256));
            land_file(&r, "ok1", FrameOrigin::Replica, &pattern(41, 256));
            {
                let conn = db(&r.ctx).unwrap().conn();
                conn.execute(
                    "UPDATE project_frames_local SET content_version = 2, on_disk = 0,
                         size_mtime_seen = NULL WHERE frame_uuid = 'vp'",
                    [],
                )
                .unwrap();
                conn.execute(
                    "UPDATE project_frames_local SET on_disk = 0 WHERE frame_uuid = 'ok1'",
                    [],
                )
                .unwrap();
            }

            resolve_collab_loss(
                Arc::clone(&r.ctx),
                Arc::new(crate::sync::SyncRuntime::new()),
                PID,
                LossAction::StopHolding,
                None,
            )
            .await
            .unwrap();

            assert!(!paused(&r));
            for row in rows(&r.ctx) {
                let lost = row.frame_uuid == "f00" || row.frame_uuid == "f01";
                assert_eq!(row.locally_declined, lost, "{}", row.frame_uuid);
            }
            assert!(
                resolve_collab_loss(
                    Arc::clone(&r.ctx),
                    Arc::new(crate::sync::SyncRuntime::new()),
                    "no-such-project",
                    LossAction::StopHolding,
                    None,
                )
                .await
                .is_err(),
                "an unknown project is refused"
            );
        }

        /// P14 "Restore": after the user moved a publisher folder inside the
        /// Collaboration root, the rescan repairs every moved replica's path
        /// (the scanner's moved branch over `project_frames_local`, P26) and
        /// replication resumes. Nothing is catalogued or listed foreign.
        #[tokio::test]
        async fn restore_after_a_folder_move_repairs_paths_and_unpauses() {
            let r = rfx("send_receive").await;
            let a = land_file(&r, "a", FrameOrigin::Replica, &pattern(51, 256));
            let b = land_file(&r, "b", FrameOrigin::Replica, &pattern(52, 256));
            let moved = r.collab.join("m31").join("moved");
            std::fs::rename(a.parent().unwrap(), &moved).unwrap();
            {
                let conn = db(&r.ctx).unwrap().conn();
                crate::db::collab::set_replication_paused(&conn, PID, true).unwrap();
            }
            assert!(paused(&r));

            resolve_collab_loss(
                Arc::clone(&r.ctx),
                Arc::new(crate::sync::SyncRuntime::new()),
                PID,
                LossAction::Restore,
                None,
            )
            .await
            .unwrap();

            assert!(!paused(&r), "restore unpauses");
            for (uuid, old) in [("a", &a), ("b", &b)] {
                let got = row(&r.ctx, uuid).unwrap();
                let want = moved.join(old.file_name().unwrap());
                assert_eq!(
                    got.landed_path.as_deref(),
                    Some(want.to_string_lossy().as_ref()),
                    "{uuid}: path repaired"
                );
                assert!(got.on_disk, "{uuid}: present again");
            }
            let conn = db(&r.ctx).unwrap().conn();
            let (files, foreign): (i64, i64) = conn
                .query_row(
                    "SELECT (SELECT COUNT(*) FROM files),
                            (SELECT COUNT(*) FROM collab_foreign_files)",
                    [],
                    |q| Ok((q.get(0)?, q.get(1)?)),
                )
                .unwrap();
            assert_eq!((files, foreign), (0, 0));
        }

        // ── holder reports ───────────────────────────────────────────────────

        fn holder_puts(reqs: &[wiremock::Request]) -> Vec<serde_json::Value> {
            reqs.iter()
                .filter(|q| q.method.as_str() == "PUT" && q.url.path().ends_with("/holders/self"))
                .map(|q| q.body_json::<serde_json::Value>().unwrap())
                .collect()
        }

        /// F5 regression: the full report comes from the disk, so a deleted
        /// own file stops being a holder at the hub on the next cadence pass.
        #[tokio::test]
        async fn full_report_carries_only_present_frames() {
            let r = rfx("send_receive").await;
            r.hub.seed_frames(PID, "acc-me", &["f1", "f2"], "published");
            sync_rows(&r).await;
            let me = B64.encode(own_node_for(&r.ctx));
            assert_eq!(
                r.hub.holders_of(PID, "f2"),
                vec![me.clone()],
                "announce made me a holder"
            );
            let dir = r.collab.join("m31").join("me");
            std::fs::create_dir_all(&dir).unwrap();
            for uuid in ["f1", "f2"] {
                let path = dir.join(format!("{uuid}.fits"));
                std::fs::write(&path, uuid).unwrap();
                let sm = size_mtime_from(&std::fs::metadata(&path).unwrap());
                frames_db::adopt_own(
                    &db(&r.ctx).unwrap().conn(),
                    PID,
                    uuid,
                    1,
                    &path.to_string_lossy(),
                    "recipe",
                    Some(&sm),
                )
                .unwrap();
            }
            std::fs::remove_file(dir.join("f2.fits")).unwrap();

            let from = request_count(&r.hub).await;
            let out = run_maintenance(&r.ctx, None, None).await;
            assert_eq!(out.reported, 1);
            let reqs = r.hub.server.received_requests().await.unwrap();
            let puts = holder_puts(&reqs[from..]);
            assert_eq!(puts.len(), 1);
            assert_eq!(puts[0]["full"], true);
            assert_eq!(
                puts[0]["add"],
                serde_json::json!([{ "frameUuid": "f1", "contentVersion": 1 }])
            );
            assert!(r.hub.holders_of(PID, "f2").is_empty(), "no phantom holder");
            assert_eq!(r.hub.holders_of(PID, "f1"), vec![me]);
        }

        /// P8: above 10 000 held frames the first chunk goes `full`, the rest
        /// as `add`.
        #[tokio::test]
        async fn over_ten_thousand_frames_chunks_full_then_add() {
            let r = rfx("send_receive").await;
            let present: Vec<(String, i32)> =
                (0..10_005).map(|i| (format!("u{i:05}"), 1)).collect();
            let from = request_count(&r.hub).await;
            assert_eq!(report_holders(&r.ctx, PID, &present).await.unwrap(), 10_005);
            let reqs = r.hub.server.received_requests().await.unwrap();
            let puts = holder_puts(&reqs[from..]);
            assert_eq!(puts.len(), 2);
            assert_eq!(puts[0]["full"], true);
            assert_eq!(puts[0]["add"].as_array().unwrap().len(), 10_000);
            assert_eq!(puts[1]["full"], false);
            assert_eq!(puts[1]["add"].as_array().unwrap().len(), 5);
            assert_eq!(puts[1]["add"][0]["frameUuid"], "u10000");
            assert_eq!(puts[1]["remove"], serde_json::json!([]));
        }

        // ── pass kinds and gates ─────────────────────────────────────────────

        /// Spec §5.5 / R15 / R18: a fetch pass (timer or kick) does
        /// manifest-driven work only — no stat walk, no full holder report.
        /// The maintenance does both.
        #[tokio::test]
        async fn a_fetch_pass_walks_no_disk_and_sends_no_full_report() {
            let r = rfx("send_receive").await;
            r.hub.seed_frames(PID, "acc-o", &["n1"], "published");
            sync_rows(&r).await;
            let path = land_file(&r, "f1", FrameOrigin::Replica, &pattern(1, 256));
            std::fs::remove_file(&path).unwrap();

            let rec = Arc::new(FetchRecorder::default());
            let from = request_count(&r.hub).await;
            let out = pass_with(&r.ctx, &rec, PassKind::Fetch, None, None).await;
            assert_eq!(out.reported, 0);
            assert!(row(&r.ctx, "f1").unwrap().on_disk, "no stat walk");
            let reqs = r.hub.server.received_requests().await.unwrap();
            assert!(holder_puts(&reqs[from..]).is_empty(), "no full report");
            assert_eq!(
                *rec.calls.lock().unwrap(),
                vec![(PID.to_string(), vec!["n1".to_string()])],
                "the need set still goes to the fetch"
            );

            let m = run_maintenance(&r.ctx, None, None).await;
            assert_eq!(m.reported, 1);
            assert!(!row(&r.ctx, "f1").unwrap().on_disk);
            let reqs = r.hub.server.received_requests().await.unwrap();
            assert_eq!(holder_puts(&reqs[from..]).len(), 1);
        }

        /// A `send` member replicates nothing (but still reports what it
        /// holds); a project with auto-replicate off is skipped by a fetch
        /// pass and synced by a forced one.
        #[tokio::test]
        async fn the_role_gate_holds_and_only_a_forced_pass_overrides_the_toggle() {
            let send = rfx("send").await;
            send.hub.seed_frames(PID, "acc-o", &["n1"], "published");
            sync_rows(&send).await;
            let rec = Arc::new(FetchRecorder::default());
            assert_eq!(
                run_maintenance(&send.ctx, None, None).await.reported,
                1,
                "a send member still reports its holds"
            );
            let out = pass_with(&send.ctx, &rec, PassKind::Forced, None, None).await;
            assert_eq!(out.reported, 1, "a forced pass runs the maintenance first");
            assert!(rec.projects().is_empty(), "the role gate is never forced");

            let off = rfx("send_receive").await;
            off.hub.seed_frames(PID, "acc-o", &["n1"], "published");
            sync_rows(&off).await;
            set_project_auto_replicate(&off.ctx, PID, false).unwrap();
            let rec = Arc::new(FetchRecorder::default());
            pass_with(&off.ctx, &rec, PassKind::Fetch, None, None).await;
            assert!(rec.projects().is_empty(), "the toggle is off");
            let out = pass_with(&off.ctx, &rec, PassKind::Forced, Some(PID), None).await;
            assert_eq!(rec.projects(), vec![PID.to_string()]);
            assert_eq!(out.projects, 1);
            let out = pass_with(&off.ctx, &rec, PassKind::Forced, Some("other"), None).await;
            assert_eq!(out.projects, 0, "a scoped pass touches only its project");
        }

        /// Signed out, no Collaboration root, or a root that is not
        /// reachable (m5): the pass and the maintenance do nothing.
        #[tokio::test]
        async fn the_pass_is_a_no_op_signed_out_or_without_a_collaboration_root() {
            let Fx {
                tmp: _tmp,
                ctx,
                hub,
            } = fx("send_receive", false, false).await;
            crate::api::collab::refresh_projects(&ctx).await.unwrap();
            hub.seed_frames(PID, "acc-o", &["n1"], "published");
            sync_manifest(&ctx, PID, None, None).await.unwrap();
            let rec = Arc::new(FetchRecorder::default());
            let from = request_count(&hub).await;
            let out = pass_with(&ctx, &rec, PassKind::Fetch, None, None).await;
            assert_eq!(out, AutoSyncPassOutcome::default());
            assert_eq!(
                run_maintenance(&ctx, None, None).await,
                MaintenanceOutcome::default()
            );
            assert!(rec.projects().is_empty());
            assert_eq!(
                request_count(&hub).await,
                from,
                "no hub call without a root"
            );

            let r = rfx("send_receive").await;
            {
                // A hub this device holds no token for = signed out.
                let conn = db(&r.ctx).unwrap().conn();
                crate::db::set_setting(
                    &conn,
                    crate::settings::keys::ACCOUNT_HUB_URL,
                    "http://signed-out.invalid",
                )
                .unwrap();
            }
            let out = pass_with(&r.ctx, &rec, PassKind::Fetch, None, None).await;
            assert_eq!(out, AutoSyncPassOutcome::default());

            // Signed in, but the Collaboration folder's volume is gone: no
            // landed file may be counted missing, nothing is fetched.
            let r = rfx("send_receive").await;
            r.hub.seed_frames(PID, "acc-o", &["n1"], "published");
            sync_rows(&r).await;
            land_file(&r, "f1", FrameOrigin::Replica, &pattern(1, 256));
            std::fs::remove_dir_all(&r.collab).unwrap();
            assert_eq!(
                run_maintenance(&r.ctx, None, None).await,
                MaintenanceOutcome::default()
            );
            let out = pass_with(&r.ctx, &rec, PassKind::Forced, None, None).await;
            assert_eq!(out, AutoSyncPassOutcome::default());
            assert!(rec.projects().is_empty());
            assert!(row(&r.ctx, "f1").unwrap().on_disk, "not counted missing");
        }

        /// One project's failing fetch never ends the pass.
        #[tokio::test]
        async fn a_failing_fetch_does_not_end_the_pass() {
            let r = rfx("send_receive").await;
            r.hub.add_project(
                "p2",
                "m42",
                &[
                    ("acc-me", "send_receive", false),
                    ("acc-o", "send_receive", false),
                ],
                false,
            );
            crate::api::collab::refresh_projects(&r.ctx).await.unwrap();
            r.hub.seed_frames(PID, "acc-o", &["n1"], "published");
            r.hub.seed_frames("p2", "acc-o", &["n2"], "published");
            sync_rows(&r).await;
            sync_manifest(&r.ctx, "p2", None, None).await.unwrap();
            let rec = Arc::new(FetchRecorder::default());
            rec.fail.lock().unwrap().insert(PID.to_string());
            let out = pass_with(&r.ctx, &rec, PassKind::Fetch, None, None).await;
            let mut projects = rec.projects();
            projects.sort();
            assert_eq!(projects, vec![PID.to_string(), "p2".to_string()]);
            assert_eq!(out.failed, 1);
            assert_eq!(out.landed, 1);
        }

        // ── the policy commands ──────────────────────────────────────────────

        /// `set_collab_policy` stores the policy and returns what it selects;
        /// a preview stores nothing; a bad policy is refused.
        #[tokio::test]
        async fn set_policy_returns_a_preview_and_persists() {
            let r = rfx("send_receive").await;
            r.hub
                .seed_frames(PID, "acc-o", &["a", "b", "c"], "published");
            r.hub
                .update_frame(PID, "c", |f| f.filter_canonical = "Ha".into());
            sync_rows(&r).await;
            let held = land_file(&r, "a", FrameOrigin::Replica, &pattern(1, 1000));
            let _ = held;

            assert_eq!(
                get_collab_policy(&r.ctx, PID).await.unwrap(),
                ReplicationPolicy::default()
            );
            let l_only = ReplicationPolicy {
                filters: vec!["L".into()],
                ..Default::default()
            };
            let preview = set_collab_policy(&r.ctx, PID, l_only.clone())
                .await
                .unwrap();
            assert_eq!(
                preview,
                PolicyPreview {
                    frames: 2,
                    bytes: 2000,
                    already_held: 1,
                    to_fetch: 1,
                    to_fetch_bytes: 1000,
                }
            );
            assert_eq!(get_collab_policy(&r.ctx, PID).await.unwrap(), l_only);

            let ha = ReplicationPolicy {
                filters: vec!["Ha".into()],
                ..Default::default()
            };
            let p = preview_collab_policy(&r.ctx, PID, ha).await.unwrap();
            assert_eq!((p.frames, p.to_fetch), (1, 1));
            assert_eq!(
                get_collab_policy(&r.ctx, PID).await.unwrap(),
                l_only,
                "a preview stores nothing"
            );

            let bad = ReplicationPolicy {
                byte_budget: Some(-1),
                ..Default::default()
            };
            assert!(matches!(
                set_collab_policy(&r.ctx, PID, bad).await,
                Err(ApiError::Invalid(_))
            ));
            assert!(get_collab_policy(&r.ctx, "nope").await.is_err());
        }

        // ── real iroh: fetch, land, seed ─────────────────────────────────────

        /// P21 end to end: one pass fetches a 4 MiB frame from its only
        /// holder and lands it by RENAME — the store keeps only its outboard —
        /// under `<Collab>/<project>/<publisher>/`; the landed file is the
        /// seed, so a third device fetches it from the receiver alone.
        #[tokio::test]
        async fn frame_lands_by_rename_and_becomes_the_seed() {
            const SIZE: usize = 4 * 1024 * 1024;
            let r = rfx("send_receive").await;
            let recv = bind_receiver(&r).await;
            let pub_dir = tempfile::tempdir().unwrap();
            let publisher = peer_node(pub_dir.path()).await;
            pair(&recv, &publisher).await;
            let bytes = pattern(7, SIZE);
            let hash = publish_on(&r, &publisher, "f1", "c_x.fits", &bytes).await;
            sync_rows(&r).await;

            let out = real_cycle(&r).await;
            assert_eq!((out.landed, out.failed), (1, 0), "{out:?}");

            let dest = r.collab.join("m31").join("other").join("c_x.fits");
            assert_eq!(std::fs::read(&dest).unwrap(), bytes, "identical bytes");
            let row = row(&r.ctx, "f1").unwrap();
            assert!(row.on_disk);
            assert_eq!(
                row.landed_path.as_deref(),
                Some(dest.to_string_lossy().as_ref())
            );
            let store_data = dir_bytes(&r.collab.join(".athenaeum").join("blobs").join("data"));
            assert!(
                store_data < (SIZE as u64) / 100,
                "the store's data was renamed out, not copied: {store_data} B left"
            );
            assert_eq!(collab_tags(&recv, "project/p1/f1/1").await, 1, "seeded");
            assert_eq!(collab_tags(&recv, "in-flight/").await, 0);
            assert!(
                r.hub
                    .holders_of(PID, "f1")
                    .contains(&B64.encode(own_node_for(&r.ctx))),
                "the landing is reported as a holder delta"
            );

            // A third device, with the publisher gone.
            let third_dir = tempfile::tempdir().unwrap();
            let third = peer_node(third_dir.path()).await;
            pair(&third, &recv).await;
            publisher.shutdown().await;
            let t_store = third.collab_store().unwrap();
            let results = tokio::time::timeout(
                Duration::from_secs(60),
                crate::sharing::iroh::blobs::fetch_blobs_assigned(
                    &t_store,
                    &third.endpoint(),
                    vec![crate::sharing::iroh::blobs::FrameFetch {
                        key: "f1".into(),
                        hash,
                        size: SIZE as u64,
                        providers: vec![iroh::EndpointId::from_bytes(&recv.node_id()).unwrap()],
                        in_flight_tag: "in-flight/project/p1/f1/1".into(),
                    }],
                    Arc::new(|_| {}),
                ),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(results[0].1.is_ok(), "{:?}", results[0].1);
            assert!(t_store.blobs().has(hash).await.unwrap());
            third.shutdown().await;
            recv.shutdown().await;
        }

        /// P20: a deleted landing is parked until GC — no fetch over the dead
        /// entry (iroh-blobs 0.103 would panic) — and fetched again once GC
        /// dropped the entry.
        #[tokio::test]
        async fn second_pass_after_delete_waits_for_gc_then_refetches() {
            use std::sync::atomic::{AtomicBool, Ordering};
            const SIZE: usize = 1024 * 1024;
            let r = rfx("send_receive").await;
            let gc_open = Arc::new(AtomicBool::new(false));
            crate::sharing::iroh::node::test_gc::arm(Some(Arc::clone(&gc_open)));
            let recv = bind_receiver(&r).await;
            crate::sharing::iroh::node::test_gc::arm(None);
            let pub_dir = tempfile::tempdir().unwrap();
            let publisher = peer_node(pub_dir.path()).await;
            pair(&recv, &publisher).await;
            let bytes = pattern(3, SIZE);
            let hash = publish_on(&r, &publisher, "f1", "c_x.fits", &bytes).await;
            sync_rows(&r).await;
            assert_eq!(real_cycle(&r).await.landed, 1);

            let dest = PathBuf::from(row(&r.ctx, "f1").unwrap().landed_path.unwrap());
            std::fs::remove_file(&dest).unwrap();
            let before = sent_bytes(&publisher);
            let out = real_cycle(&r).await;
            assert_eq!(out.attempted, 0, "{out:?}");
            let row1 = row(&r.ctx, "f1").unwrap();
            assert!(!row1.on_disk && row1.awaiting_gc);
            let served = sent_bytes(&publisher).saturating_sub(before);
            assert!(
                served < SERVED_PAYLOAD_FLOOR,
                "no request reached the provider: it sent {served} B"
            );

            gc_open.store(true, Ordering::SeqCst);
            let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
            while recv.collab_blob_health(hash).await.unwrap() != BlobHealth::Missing {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "GC never dropped the entry"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            gc_open.store(false, Ordering::SeqCst);

            let out = real_cycle(&r).await;
            assert_eq!(out.landed, 1, "{out:?}");
            assert_eq!(std::fs::read(&dest).unwrap(), bytes, "the frame is back");
            assert!(row(&r.ctx, "f1").unwrap().on_disk);
            publisher.shutdown().await;
            recv.shutdown().await;
        }

        /// Plan step 7.5 (owner-approved): a new content version lands OVER
        /// the old file under the same name — never `c_x_2.fits` — and the
        /// old version's seed tag goes.
        #[tokio::test]
        async fn a_new_version_lands_over_the_old_file() {
            let r = rfx("send_receive").await;
            let recv = bind_receiver(&r).await;
            let pub_dir = tempfile::tempdir().unwrap();
            let publisher = peer_node(pub_dir.path()).await;
            pair(&recv, &publisher).await;
            let v1 = pattern(1, 256 * 1024);
            publish_on(&r, &publisher, "f1", "c_x.fits", &v1).await;
            sync_rows(&r).await;
            assert_eq!(real_cycle(&r).await.landed, 1);
            let dest = r.collab.join("m31").join("other").join("c_x.fits");
            assert_eq!(std::fs::read(&dest).unwrap(), v1);

            // The publisher re-calibrates: version 2, new bytes, re-seeded and
            // reported as the v2 holder.
            let v2 = pattern(2, 256 * 1024);
            let v2_path = pub_dir.path().join("c_x_v2.fits");
            std::fs::write(&v2_path, &v2).unwrap();
            let h2 = publisher
                .seed_project_frame(PID, "f1", 2, &v2_path)
                .await
                .unwrap();
            let x2 = hash_bytes(&v2);
            r.hub.update_frame(PID, "f1", move |f| {
                f.content_version = 2;
                f.blake3 = h2.to_hex().to_string();
                f.xxh3 = x2;
            });
            crate::collab::hub_client::CollabClient::new(&r.hub.uri())
                .unwrap()
                .put_holders(
                    "tok-o",
                    PID,
                    false,
                    &[crate::collab::hub_client::HolderRefWire {
                        frame_uuid: "f1".into(),
                        content_version: 2,
                    }],
                    &[],
                )
                .await
                .unwrap();
            sync_rows(&r).await;
            assert!(
                !row(&r.ctx, "f1").unwrap().on_disk,
                "a new version is needed"
            );

            let out = real_pass(&r, PassKind::Fetch).await;
            assert_eq!(out.landed, 1, "{out:?}");
            assert_eq!(std::fs::read(&dest).unwrap(), v2, "v2 over v1, same path");
            assert!(!r
                .collab
                .join("m31")
                .join("other")
                .join("c_x_2.fits")
                .exists());
            assert_eq!(
                collab_tags(&recv, "project/p1/f1/1").await,
                0,
                "v1 unseeded"
            );
            assert_eq!(collab_tags(&recv, "project/p1/f1/2").await, 1);
            let row = row(&r.ctx, "f1").unwrap();
            assert!(row.on_disk);
            assert_eq!(row.content_version, 2);
            publisher.shutdown().await;
            recv.shutdown().await;
        }

        /// P11 isolation: a frame no holder can serve fails alone; its sibling
        /// lands. The failed frame keeps its in-flight tag while it is still
        /// wanted and loses it to the maintenance sweep once it is not (R23).
        #[tokio::test]
        async fn fetch_failure_of_one_frame_lands_the_others() {
            let r = rfx("send_receive").await;
            let recv = bind_receiver(&r).await;
            let pub_dir = tempfile::tempdir().unwrap();
            let publisher = peer_node(pub_dir.path()).await;
            pair(&recv, &publisher).await;
            let good = pattern(1, 128 * 1024);
            publish_on(&r, &publisher, "good", "good.fits", &good).await;
            // Listed with the publisher as holder, but never seeded there.
            let ghost = pattern(2, 128 * 1024);
            r.hub.seed_frames(PID, "acc-o", &["ghost"], "published");
            let (gh, gx) = (
                blake3::hash(&ghost).to_hex().to_string(),
                hash_bytes(&ghost),
            );
            r.hub.update_frame(PID, "ghost", move |f| {
                f.blake3 = gh;
                f.xxh3 = gx;
                f.byte_size = 128 * 1024;
            });
            sync_rows(&r).await;

            let out = real_cycle(&r).await;
            assert_eq!((out.landed, out.failed), (1, 1), "{out:?}");
            assert!(row(&r.ctx, "good").unwrap().on_disk);
            let ghost_row = row(&r.ctx, "ghost").unwrap();
            assert!(!ghost_row.on_disk);
            assert!(ghost_row.last_error.is_some());
            assert_eq!(
                collab_tags(&recv, "in-flight/").await,
                1,
                "a transfer failure keeps its in-flight tag for the resume (R23)"
            );
            assert_eq!(collab_tags(&recv, "in-flight/project/p1/ghost/1").await, 1);

            // Still wanted: the maintenance sweep keeps it.
            run_maintenance(&r.ctx, None, None).await;
            assert_eq!(collab_tags(&recv, "in-flight/").await, 1);
            // Declined: the frame left the need set, the sweep drops the tag.
            frames_db::set_declined(
                &db(&r.ctx).unwrap().conn(),
                PID,
                &["ghost".to_string()],
                true,
            )
            .unwrap();
            run_maintenance(&r.ctx, None, None).await;
            assert_eq!(collab_tags(&recv, "in-flight/").await, 0);
            publisher.shutdown().await;
            recv.shutdown().await;
        }

        /// P24: a second frame with the content of one already landed is
        /// linked from that file — no bytes move — and seeded.
        #[tokio::test]
        async fn identical_content_second_frame_is_linked_not_fetched() {
            const SIZE: usize = 1024 * 1024;
            let r = rfx("send_receive").await;
            let recv = bind_receiver(&r).await;
            let pub_dir = tempfile::tempdir().unwrap();
            let publisher = peer_node(pub_dir.path()).await;
            pair(&recv, &publisher).await;
            let bytes = pattern(5, SIZE);
            let hash = publish_on(&r, &publisher, "f1", "c_x.fits", &bytes).await;
            sync_rows(&r).await;
            assert_eq!(real_cycle(&r).await.landed, 1);

            r.hub.seed_frames(PID, "acc-o", &["f2"], "published");
            let x = hash_bytes(&bytes);
            r.hub.update_frame(PID, "f2", move |f| {
                f.file_name = "c_y.fits".into();
                f.blake3 = hash.to_hex().to_string();
                f.xxh3 = x;
                f.byte_size = SIZE as i64;
            });
            sync_rows(&r).await;
            let before = sent_bytes(&publisher);
            let out = real_pass(&r, PassKind::Fetch).await;
            assert_eq!(out.landed, 1, "{out:?}");
            let dest = r.collab.join("m31").join("other").join("c_y.fits");
            assert_eq!(std::fs::read(&dest).unwrap(), bytes);
            assert!(row(&r.ctx, "f2").unwrap().on_disk);
            assert_eq!(collab_tags(&recv, "project/p1/f2/1").await, 1);
            let served = sent_bytes(&publisher).saturating_sub(before);
            assert!(
                served < SERVED_PAYLOAD_FLOOR,
                "linked, not fetched: {served} B"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                let first = r.collab.join("m31").join("other").join("c_x.fits");
                assert_eq!(
                    std::fs::metadata(&dest).unwrap().ino(),
                    std::fs::metadata(&first).unwrap().ino(),
                    "P24 links the landed file (same volume), never a second copy"
                );
            }

            // Two identical frames needed in ONE batch: one fetch, two landings.
            let twin = pattern(6, SIZE);
            let twin_hash = publish_on(&r, &publisher, "t1", "t_1.fits", &twin).await;
            r.hub.seed_frames(PID, "acc-o", &["t2"], "published");
            let x = hash_bytes(&twin);
            r.hub.update_frame(PID, "t2", move |f| {
                f.file_name = "t_2.fits".into();
                f.blake3 = twin_hash.to_hex().to_string();
                f.xxh3 = x;
                f.byte_size = SIZE as i64;
            });
            sync_rows(&r).await;
            let before = sent_bytes(&publisher);
            let out = real_pass(&r, PassKind::Fetch).await;
            assert_eq!((out.landed, out.failed), (2, 0), "{out:?}");
            for name in ["t_1.fits", "t_2.fits"] {
                let p = r.collab.join("m31").join("other").join(name);
                assert_eq!(std::fs::read(&p).unwrap(), twin, "{name}");
            }
            let served = sent_bytes(&publisher).saturating_sub(before);
            assert!(
                served < (SIZE as u64) * 3 / 2,
                "the content moved once for both frames: {served} B"
            );
            publisher.shutdown().await;
            recv.shutdown().await;
        }

        // ── fix round 1 (R16–R24, m5, m10) ───────────────────────────────────

        /// `acc-o` publishes every uuid (1 KiB each, distinct bytes), seeded
        /// on `publisher`, listed by the hub in one batch.
        pub(super) async fn publish_many(
            r: &RFx,
            publisher: &Arc<SharedIrohNode>,
            uuids: &[String],
            size: usize,
        ) {
            set_publisher_key(r, publisher);
            let dir = r.tmp.path().join("pub-many");
            std::fs::create_dir_all(&dir).unwrap();
            let mut facts = Vec::new();
            for (i, u) in uuids.iter().enumerate() {
                let bytes = pattern(1000 + i, size);
                let path = dir.join(format!("{u}.fits"));
                std::fs::write(&path, &bytes).unwrap();
                let hash = publisher
                    .seed_project_frame(PID, u, 1, &path)
                    .await
                    .unwrap();
                facts.push((u.clone(), hash.to_hex().to_string(), hash_bytes(&bytes)));
            }
            let refs: Vec<&str> = uuids.iter().map(String::as_str).collect();
            r.hub.seed_frames(PID, "acc-o", &refs, "published");
            let mut st = r.hub.state.lock().unwrap();
            let p = st.projects.get_mut(PID).unwrap();
            for (u, b3, x) in facts {
                let f = p.frames.get_mut(&u).unwrap();
                f.file_name = format!("{u}.fits");
                f.blake3 = b3;
                f.xxh3 = x;
                f.byte_size = size as i64;
            }
        }

        /// A receiver, a publisher and `n` published frames: `f000…` and a
        /// last one, `z-last`, which sorts after every other.
        pub(super) async fn many_rig(
            n: usize,
        ) -> (
            RFx,
            Arc<SharedIrohNode>,
            Arc<SharedIrohNode>,
            tempfile::TempDir,
        ) {
            let r = rfx("send_receive").await;
            let recv = bind_receiver(&r).await;
            let pub_dir = tempfile::tempdir().unwrap();
            let publisher = peer_node(pub_dir.path()).await;
            pair(&recv, &publisher).await;
            let mut uuids: Vec<String> = (0..n - 1).map(|i| format!("f{i:03}")).collect();
            uuids.push("z-last".into());
            publish_many(&r, &publisher, &uuids, 1024).await;
            sync_rows(&r).await;
            (r, recv, publisher, pub_dir)
        }

        /// The need set through the gated fetch, as the pass would run it.
        pub(super) async fn fetch_all(
            r: &RFx,
            gate: Option<&crate::sync::ReceiveGate>,
        ) -> FetchOutcome {
            let need = frame_need(
                &rows(&r.ctx),
                &ReplicationPolicy::default(),
                true,
                true,
                false,
            );
            tokio::time::timeout(
                Duration::from_secs(120),
                fetch_frames_gated(&r.ctx, gate, PID, need, false, None),
            )
            .await
            .expect("a fetch must not take two minutes")
            .unwrap()
        }

        pub(super) async fn holder_lookups(r: &RFx, uuid: &str) -> usize {
            let suffix = format!("/frames/{uuid}/holders");
            r.hub
                .server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .filter(|q| q.url.path().ends_with(&suffix))
                .count()
        }

        /// A second catalog connection (another writer, e.g. a manifest sync
        /// on the poll's task).
        pub(super) fn side_conn(r: &RFx) -> rusqlite::Connection {
            let c = rusqlite::Connection::open(db(&r.ctx).unwrap().path()).unwrap();
            c.busy_timeout(Duration::from_secs(5)).unwrap();
            c
        }

        /// R16: frames no holder can serve never take a batch slot — 201 of
        /// them ahead (rarest first: nobody holds them) must not stop the one
        /// servable frame behind them from landing in the same pass.
        #[tokio::test]
        async fn unservable_frames_ahead_never_block_a_servable_one() {
            let r = rfx("send_receive").await;
            let recv = bind_receiver(&r).await;
            let pub_dir = tempfile::tempdir().unwrap();
            let publisher = peer_node(pub_dir.path()).await;
            pair(&recv, &publisher).await;
            // Published by an account with no device: no holder, ever.
            let orphans: Vec<String> = (0..=FETCH_BATCH).map(|i| format!("o{i:03}")).collect();
            let refs: Vec<&str> = orphans.iter().map(String::as_str).collect();
            r.hub.seed_frames(PID, "acc-x", &refs, "published");
            publish_on(&r, &publisher, "good", "good.fits", &pattern(3, 4096)).await;
            sync_rows(&r).await;
            let need = frame_need(
                &rows(&r.ctx),
                &ReplicationPolicy::default(),
                true,
                true,
                false,
            );
            assert_eq!(
                need.last().unwrap().frame_uuid,
                "good",
                "the orphans come first"
            );

            let out = real_pass(&r, PassKind::Fetch).await;
            assert_eq!((out.landed, out.failed), (1, 0), "{out:?}");
            assert!(row(&r.ctx, "good").unwrap().on_disk);
            publisher.shutdown().await;
            recv.shutdown().await;
        }

        /// R17: the receive permit is taken per batch. A personal-sync
        /// receive that asks for the (one-lane) gate while the second batch
        /// is being prepared gets it — and finishes — before the fetch ends.
        #[tokio::test]
        async fn a_personal_receive_gets_the_gate_between_batches() {
            let (r, recv, publisher, _pub_dir) = many_rig(FETCH_BATCH + 1).await;
            let gate = Arc::new(crate::sync::ReceiveGate::new(1));
            let acquired = Arc::new(std::sync::Mutex::new(None::<std::time::Instant>));
            let released = Arc::new(std::sync::Mutex::new(None::<std::time::Instant>));
            {
                let handle = tokio::runtime::Handle::current();
                let (gate, acquired, released) = (
                    Arc::clone(&gate),
                    Arc::clone(&acquired),
                    Arc::clone(&released),
                );
                r.hub.before_next("/frames/z-last/holders", move |_| {
                    handle.spawn(async move {
                        let permit = gate.acquire().await;
                        *acquired.lock().unwrap() = Some(std::time::Instant::now());
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        drop(permit);
                        *released.lock().unwrap() = Some(std::time::Instant::now());
                    });
                });
            }
            let out = fetch_all(&r, Some(&gate)).await;
            let end = std::time::Instant::now();
            assert_eq!(out.landed, FETCH_BATCH + 1, "{out:?}");
            assert!(
                acquired.lock().unwrap().is_some(),
                "the personal receive got a lane"
            );
            let released = released
                .lock()
                .unwrap()
                .expect("…and was done with it before the collab fetch ended");
            assert!(released <= end);
            publisher.shutdown().await;
            recv.shutdown().await;
        }

        /// R18: the maintenance never waits for a fetch — while one is in
        /// flight (held here by the seam), a maintenance run still walks the
        /// disk and sends the full holder report.
        #[tokio::test]
        async fn maintenance_reports_while_a_fetch_is_in_flight() {
            let r = rfx("send_receive").await;
            r.hub.seed_frames(PID, "acc-o", &["n1"], "published");
            sync_rows(&r).await;
            land_file(&r, "mine", FrameOrigin::Own, &pattern(1, 256));
            let entered = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            let from = request_count(&r.hub).await;
            let pass = {
                let (entered, release) = (Arc::clone(&entered), Arc::clone(&release));
                run_auto_sync_pass(&r.ctx, PassKind::Fetch, None, None, move |_pid, _need| {
                    let (entered, release) = (Arc::clone(&entered), Arc::clone(&release));
                    async move {
                        entered.notify_one();
                        release.notified().await;
                        Ok(FetchOutcome::default())
                    }
                })
            };
            let maintenance = async {
                entered.notified().await;
                let m = tokio::time::timeout(
                    Duration::from_secs(10),
                    run_maintenance(&r.ctx, None, None),
                )
                .await
                .expect("the maintenance never waits for a running fetch");
                release.notify_one();
                m
            };
            let (_out, m) = tokio::join!(pass, maintenance);
            assert_eq!(m.reported, 1);
            let reqs = r.hub.server.received_requests().await.unwrap();
            let puts = holder_puts(&reqs[from..]);
            assert_eq!(puts.len(), 1);
            assert_eq!(puts[0]["full"], true);
        }

        /// R19: a cancel lands between batches — the running batch finishes,
        /// the next one never starts.
        #[tokio::test]
        async fn a_cancel_stops_the_fetch_between_batches() {
            let (r, recv, publisher, _pub_dir) = many_rig(FETCH_BATCH + 1).await;
            let was_running = Arc::new(std::sync::atomic::AtomicBool::new(false));
            {
                let (ctx, was_running) = (Arc::clone(&r.ctx), Arc::clone(&was_running));
                r.hub.before_next("/frames/f000/holders", move |_| {
                    was_running.store(
                        cancel_project_fetch(&ctx, PID),
                        std::sync::atomic::Ordering::SeqCst,
                    );
                });
            }
            let out = fetch_all(&r, None).await;
            assert!(was_running.load(std::sync::atomic::Ordering::SeqCst));
            assert_eq!(out.landed, FETCH_BATCH, "the first batch finishes");
            assert!(!row(&r.ctx, "z-last").unwrap().on_disk);
            assert_eq!(
                holder_lookups(&r, "z-last").await,
                0,
                "the second never starts"
            );
            assert!(!cancel_project_fetch(&r.ctx, PID), "unregistered once done");
            publisher.shutdown().await;
            recv.shutdown().await;
        }

        /// R19: between batches the queue is re-read — a frame declined while
        /// the first batch ran is dropped before its own batch.
        #[tokio::test]
        async fn a_frame_declined_mid_fetch_is_dropped_before_its_batch() {
            let (r, recv, publisher, _pub_dir) = many_rig(FETCH_BATCH + 1).await;
            let conn = std::sync::Mutex::new(side_conn(&r));
            r.hub.before_next("/frames/f000/holders", move |_| {
                conn.lock()
                    .unwrap()
                    .execute(
                        "UPDATE project_frames_local SET locally_declined = 1
                         WHERE frame_uuid = 'z-last'",
                        [],
                    )
                    .unwrap();
            });
            let out = fetch_all(&r, None).await;
            assert_eq!(out.landed, FETCH_BATCH);
            assert!(!row(&r.ctx, "z-last").unwrap().on_disk);
            assert_eq!(holder_lookups(&r, "z-last").await, 0);
            publisher.shutdown().await;
            recv.shutdown().await;
        }

        /// R20: a manifest sync that moves a frame to a new version while its
        /// old bytes are in flight — the landing is stale: the old bytes are
        /// never recorded as the new version, no tag and no file remain.
        #[tokio::test]
        async fn a_version_bump_while_in_flight_is_not_recorded_as_landed() {
            let r = rfx("send_receive").await;
            let recv = bind_receiver(&r).await;
            let pub_dir = tempfile::tempdir().unwrap();
            let publisher = peer_node(pub_dir.path()).await;
            pair(&recv, &publisher).await;
            publish_on(&r, &publisher, "f1", "c_x.fits", &pattern(1, 64 * 1024)).await;
            sync_rows(&r).await;
            let v2 = pattern(2, 64 * 1024);
            let (b3, x) = (blake3::hash(&v2).to_hex().to_string(), hash_bytes(&v2));
            let conn = std::sync::Mutex::new(side_conn(&r));
            r.hub.before_next("/frames/f1/holders", move |_| {
                conn.lock()
                    .unwrap()
                    .execute(
                        "UPDATE project_frames_local SET content_version = 2, blake3 = ?1,
                             xxh3 = ?2, on_disk = 0, size_mtime_seen = NULL
                         WHERE frame_uuid = 'f1'",
                        rusqlite::params![b3, x],
                    )
                    .unwrap();
            });

            let out = real_pass(&r, PassKind::Fetch).await;
            assert_eq!(out.landed, 0, "{out:?}");
            let row = row(&r.ctx, "f1").unwrap();
            assert_eq!(row.content_version, 2);
            assert!(!row.on_disk);
            assert!(row.landed_path.is_none(), "v1 bytes were not recorded");
            assert!(!r.collab.join("m31").join("other").join("c_x.fits").exists());
            assert_eq!(collab_tags(&recv, "project/p1/f1/").await, 0);
            assert_eq!(collab_tags(&recv, "in-flight/").await, 0);
            let history: i64 = db(&r.ctx)
                .unwrap()
                .conn()
                .query_row(
                    "SELECT COUNT(*) FROM sync_history WHERE frame_uuid = 'f1'",
                    [],
                    |q| q.get(0),
                )
                .unwrap();
            assert_eq!(history, 0);
            publisher.shutdown().await;
            recv.shutdown().await;
        }

        /// R21: a version-bumped replica's old file (same size) is hashed
        /// once; while its `size:mtime` stays the same it is not hashed again.
        #[tokio::test]
        async fn a_rejected_old_file_is_not_hashed_again() {
            let r = rfx("send_receive").await;
            let node = bind_receiver(&r).await;
            let path = land_file(&r, "f1", FrameOrigin::Replica, &pattern(1, 4096));
            let v2 = pattern(2, 4096);
            db(&r.ctx)
                .unwrap()
                .conn()
                .execute(
                    "UPDATE project_frames_local SET content_version = 2, blake3 = ?1,
                         xxh3 = ?2, on_disk = 0, size_mtime_seen = NULL
                     WHERE frame_uuid = 'f1'",
                    rusqlite::params![blake3::hash(&v2).to_hex().to_string(), hash_bytes(&v2)],
                )
                .unwrap();

            let t1 = disk_truth(&r.ctx, PID).await.unwrap();
            assert_eq!(t1.rehashed, 1);
            assert!(t1.present.is_empty() && t1.missing_replicas.is_empty());
            let t2 = disk_truth(&r.ctx, PID).await.unwrap();
            assert_eq!(t2.rehashed, 0, "the rejected file is not hashed again");
            bump_mtime(&path, 10);
            let t3 = disk_truth(&r.ctx, PID).await.unwrap();
            assert_eq!(t3.rehashed, 1, "a touched file is looked at again");
            node.shutdown().await;
        }

        /// m5: a replica recorded outside the current Collaboration folder
        /// counts as missing; an own frame may live anywhere (P26).
        #[tokio::test]
        async fn a_replica_outside_the_collaboration_folder_counts_missing() {
            let r = rfx("send_receive").await;
            land_file(&r, "in", FrameOrigin::Replica, &pattern(1, 256));
            let elsewhere = r.tmp.path().join("elsewhere");
            std::fs::create_dir_all(&elsewhere).unwrap();
            for (uuid, origin, seed) in [
                ("out", FrameOrigin::Replica, 2),
                ("mine", FrameOrigin::Own, 3),
            ] {
                let inside = land_file(&r, uuid, origin, &pattern(seed, 256));
                let moved = elsewhere.join(format!("{uuid}.fits"));
                std::fs::rename(&inside, &moved).unwrap();
                let sm = size_mtime_from(&std::fs::metadata(&moved).unwrap());
                let conn = db(&r.ctx).unwrap().conn();
                frames_db::update_landed_path(&conn, PID, uuid, &moved.to_string_lossy()).unwrap();
                frames_db::set_size_mtime_seen(&conn, PID, uuid, &sm).unwrap();
            }
            let truth = disk_truth(&r.ctx, PID).await.unwrap();
            assert_eq!(truth.missing_replicas, vec!["out".to_string()]);
            let mut present: Vec<String> = truth.present.into_iter().map(|(u, _)| u).collect();
            present.sort();
            assert_eq!(present, vec!["in".to_string(), "mine".to_string()]);
        }

        /// P24/P20 (m10): two own frames share one store entry; deleting the
        /// one whose path the store reads leaves the other unservable — it is
        /// parked, and re-admitted from its own intact file once GC dropped
        /// the dead entry.
        #[tokio::test]
        async fn a_survivor_of_a_deleted_identical_frame_is_parked_then_readmitted() {
            use std::sync::atomic::{AtomicBool, Ordering};
            let r = rfx("send_receive").await;
            let gc_open = Arc::new(AtomicBool::new(false));
            crate::sharing::iroh::node::test_gc::arm(Some(Arc::clone(&gc_open)));
            let node = bind_receiver(&r).await;
            crate::sharing::iroh::node::test_gc::arm(None);
            let bytes = pattern(5, 64 * 1024);
            let p1 = land_file(&r, "f1", FrameOrigin::Own, &bytes);
            let p2 = land_file(&r, "f2", FrameOrigin::Own, &bytes);
            let hash = node.seed_project_frame(PID, "f1", 1, &p1).await.unwrap();
            node.seed_project_frame(PID, "f2", 1, &p2).await.unwrap();

            std::fs::remove_file(&p1).unwrap();
            let truth = disk_truth(&r.ctx, PID).await.unwrap();
            assert_eq!(truth.missing_own, vec!["f1".to_string()]);
            assert_eq!(truth.parked, vec!["f2".to_string()]);
            assert!(
                truth.present.is_empty(),
                "f2 is not advertised while unservable"
            );
            let f2 = row(&r.ctx, "f2").unwrap();
            assert!(!f2.on_disk && f2.awaiting_gc);
            assert_eq!(collab_tags(&node, "project/p1/").await, 0);

            gc_open.store(true, Ordering::SeqCst);
            let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
            while node.collab_blob_health(hash).await.unwrap() != BlobHealth::Missing {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "GC never dropped the entry"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            gc_open.store(false, Ordering::SeqCst);

            let truth = disk_truth(&r.ctx, PID).await.unwrap();
            assert_eq!(truth.present, vec![("f2".to_string(), 1)]);
            assert!(row(&r.ctx, "f2").unwrap().on_disk);
            assert_eq!(collab_tags(&node, "project/p1/f2/1").await, 1);
            assert_eq!(
                node.collab_blob_health(hash).await.unwrap(),
                BlobHealth::Readable
            );
            node.shutdown().await;
        }

        /// R24: a same-version re-land never destroys an edited replica —
        /// the edited file is kept beside the frame fetched again.
        #[tokio::test]
        async fn an_edited_replica_is_kept_beside_the_refetched_frame() {
            use std::sync::atomic::{AtomicBool, Ordering};
            const SIZE: usize = 256 * 1024;
            let r = rfx("send_receive").await;
            let gc_open = Arc::new(AtomicBool::new(false));
            crate::sharing::iroh::node::test_gc::arm(Some(Arc::clone(&gc_open)));
            let recv = bind_receiver(&r).await;
            crate::sharing::iroh::node::test_gc::arm(None);
            let pub_dir = tempfile::tempdir().unwrap();
            let publisher = peer_node(pub_dir.path()).await;
            pair(&recv, &publisher).await;
            let original = pattern(4, SIZE);
            let hash = publish_on(&r, &publisher, "f1", "c_x.fits", &original).await;
            sync_rows(&r).await;
            assert_eq!(real_cycle(&r).await.landed, 1);

            let dest = r.collab.join("m31").join("other").join("c_x.fits");
            let edited = pattern(9, SIZE);
            std::fs::write(&dest, &edited).unwrap();
            bump_mtime(&dest, 10);
            run_maintenance(&r.ctx, None, None).await;
            assert!(
                !row(&r.ctx, "f1").unwrap().on_disk,
                "the edit counts missing"
            );

            gc_open.store(true, Ordering::SeqCst);
            let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
            while recv.collab_blob_health(hash).await.unwrap() != BlobHealth::Missing {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "GC never dropped the entry"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            gc_open.store(false, Ordering::SeqCst);

            let out = real_cycle(&r).await;
            assert_eq!(out.landed, 1, "{out:?}");
            assert_eq!(std::fs::read(&dest).unwrap(), original, "the frame is back");
            let aside = r.collab.join("m31").join("other").join("c_x_2.fits");
            assert_eq!(std::fs::read(&aside).unwrap(), edited, "the edit is kept");
            // P26/R18: the kept edit is an inert foreign file to the scanner —
            // listed, never catalogued; the re-landed frame is known.
            let root_id: i64 = db(&r.ctx)
                .unwrap()
                .conn()
                .query_row(
                    "SELECT id FROM scan_roots WHERE kind = 'collaboration'",
                    [],
                    |q| q.get(0),
                )
                .unwrap();
            let scan_ctx = Arc::clone(&r.ctx);
            tokio::task::spawn_blocking(move || {
                crate::api::scan_roots::start_scan_with_progress(
                    &scan_ctx,
                    root_id,
                    &crate::events::NullEmitter,
                )
            })
            .await
            .unwrap()
            .unwrap();
            {
                let conn = db(&r.ctx).unwrap().conn();
                let listed: Vec<String> = conn
                    .prepare("SELECT path FROM collab_foreign_files")
                    .unwrap()
                    .query_map([], |q| q.get(0))
                    .unwrap()
                    .collect::<rusqlite::Result<_>>()
                    .unwrap();
                assert_eq!(listed, vec![aside.to_string_lossy().to_string()]);
                let files: i64 = conn
                    .query_row("SELECT COUNT(*) FROM files", [], |q| q.get(0))
                    .unwrap();
                assert_eq!(files, 0, "nothing under the root is catalogued");
            }

            // N3: a re-land over a file that already IS the frame goes over
            // it — no byte-identical copy is kept aside.
            db(&r.ctx)
                .unwrap()
                .conn()
                .execute(
                    "UPDATE project_frames_local SET on_disk = 0 WHERE frame_uuid = 'f1'",
                    [],
                )
                .unwrap();
            recv.unseed_project_frame(PID, "f1").await.unwrap();
            gc_open.store(true, Ordering::SeqCst);
            let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
            while recv.collab_blob_health(hash).await.unwrap() != BlobHealth::Missing {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "GC never dropped the entry"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            gc_open.store(false, Ordering::SeqCst);
            let out = real_pass(&r, PassKind::Fetch).await;
            assert_eq!(out.landed, 1, "{out:?}");
            assert_eq!(std::fs::read(&dest).unwrap(), original);
            assert!(
                !r.collab
                    .join("m31")
                    .join("other")
                    .join("c_x_3.fits")
                    .exists(),
                "no byte-identical copy aside"
            );
            publisher.shutdown().await;
            recv.shutdown().await;
        }

        /// R25: a hub failing its holder lookups (here a 503) costs ONE
        /// lookup per pass, not one per frame — the rest of the need set
        /// waits for the next pass, untouched.
        #[tokio::test]
        async fn a_failing_hub_costs_one_holder_lookup_per_pass() {
            let r = rfx("send_receive").await;
            let recv = bind_receiver(&r).await;
            r.hub
                .seed_frames(PID, "acc-o", &["n1", "n2", "n3", "n4", "n5"], "published");
            sync_rows(&r).await;
            r.hub.set_failing("/holders", true);

            let from = request_count(&r.hub).await;
            let out = real_pass(&r, PassKind::Fetch).await;
            let reqs = r.hub.server.received_requests().await.unwrap();
            let lookups = reqs[from..]
                .iter()
                .filter(|q| q.method.as_str() == "GET" && q.url.path().ends_with("/holders"))
                .count();
            assert_eq!(lookups, 1, "one failed lookup stops the fetch");
            assert_eq!(out.failed, 1, "{out:?}");
            let errored = rows(&r.ctx)
                .iter()
                .filter(|x| x.last_error.is_some())
                .count();
            assert_eq!(errored, 1, "no mass row error");
            recv.shutdown().await;
        }

        /// R25: a 404 is about one frame (the hub no longer shows it to this
        /// device) — that frame is skipped and the next one is still looked
        /// up.
        #[tokio::test]
        async fn a_frame_the_hub_no_longer_shows_is_skipped_alone() {
            let r = rfx("send_receive").await;
            let recv = bind_receiver(&r).await;
            // Published by an account with no device: looked up, no holder.
            r.hub.seed_frames(PID, "acc-x", &["n1"], "published");
            sync_rows(&r).await;
            // A cached row the hub does not know (holder count 0: first).
            land_file(&r, "gone", FrameOrigin::Replica, &pattern(1, 256));
            db(&r.ctx)
                .unwrap()
                .conn()
                .execute(
                    "UPDATE project_frames_local SET on_disk = 0, landed_path = NULL,
                         size_mtime_seen = NULL, holder_count = 0
                     WHERE frame_uuid = 'gone'",
                    [],
                )
                .unwrap();
            let need = frame_need(
                &rows(&r.ctx),
                &ReplicationPolicy::default(),
                true,
                true,
                false,
            );
            assert_eq!(need[0].frame_uuid, "gone");

            let out = real_pass(&r, PassKind::Fetch).await;
            assert_eq!(holder_lookups(&r, "gone").await, 1);
            assert_eq!(
                holder_lookups(&r, "n1").await,
                1,
                "the next frame is still asked"
            );
            assert_eq!(out.failed, 1, "{out:?}");
            assert!(row(&r.ctx, "gone").unwrap().last_error.is_some());
            recv.shutdown().await;
        }

        /// R23: a transfer interrupted mid-frame keeps its in-flight tag and
        /// verified partial bytes; the next pass resumes instead of starting
        /// over.
        #[tokio::test]
        async fn an_interrupted_transfer_resumes_from_its_partial_bytes() {
            const SIZE: usize = 3 * 1024 * 1024;
            let r = rfx("send_receive").await;
            let recv = bind_receiver(&r).await;
            let pub_dir = tempfile::tempdir().unwrap();
            let publisher = peer_node(pub_dir.path()).await;
            pair(&recv, &publisher).await;
            let bytes = pattern(6, SIZE);
            let hash = publish_on(&r, &publisher, "f1", "c_x.fits", &bytes).await;
            sync_rows(&r).await;

            publisher.set_upload_limit(256_000);
            let killer = {
                let publisher = Arc::clone(&publisher);
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                    publisher.shutdown().await;
                })
            };
            let out = real_pass(&r, PassKind::Fetch).await;
            killer.await.unwrap();
            assert_eq!((out.landed, out.failed), (0, 1), "{out:?}");
            assert_eq!(
                recv.collab_blob_health(hash).await.unwrap(),
                BlobHealth::Partial
            );
            assert_eq!(collab_tags(&recv, "in-flight/project/p1/f1/1").await, 1);

            // The publisher comes back (same identity, same store).
            drop(publisher);
            let publisher = peer_node(pub_dir.path()).await;
            pair(&recv, &publisher).await;
            let out = real_pass(&r, PassKind::Fetch).await;
            assert_eq!(out.landed, 1, "{out:?}");
            let dest = r.collab.join("m31").join("other").join("c_x.fits");
            assert_eq!(std::fs::read(&dest).unwrap(), bytes);
            let served = sent_bytes(&publisher);
            assert!(
                served < SIZE as u64,
                "resumed, not restarted: the publisher sent {served} B of {SIZE}"
            );
            publisher.shutdown().await;
            recv.shutdown().await;
        }
    }
}

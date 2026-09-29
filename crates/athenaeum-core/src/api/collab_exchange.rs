//! The collab v3 per-frame exchange: the manifest delta into
//! `project_frames_local` (wave 2 Task 8, driven by the live feed since wave
//! 3), the collab store mount and its storage-marker check, the replication
//! policy commands, and the project-scoped WBPP export. The wave-2 version
//! poll, replication pass, maintenance loop and loss guard are retired (wave
//! 3 Task 15): the live exchange (`api::collab_live`) replaces them. The
//! package layer this module once held is retired too (wave 2 Task 12).
//!
//! Ungated (no render gate) except where an item says otherwise: depends only
//! on `db`, `sync`, `sharing`, `collab`, `package`, so it compiles in the
//! headless (`--no-default-features`) build.

use std::collections::{HashMap, HashSet};
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

/// Map an [`AccountClientError`](crate::account::AccountClientError) onto the api
/// boundary (mirrors `api::collab::client_err`). `pub(crate)`: the feed
/// applier (`api::collab_live::feed`) maps a re-announce failure through the
/// same rules rather than duplicating them.
pub(crate) fn client_err(e: crate::account::AccountClientError) -> ApiError {
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
        E::NotFound(m) => ApiError::NotFound(m),
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
        E::Http { message, .. } | E::Gone(message) | E::Decode(message) => {
            ApiError::Internal(format!("Hub request failed: {message}"))
        }
        E::SessionGone => ApiError::Internal("hub session expired".into()),
        E::VersionConflict { content_version } => ApiError::Conflict(format!(
            "version_conflict: the hub has content version {content_version}"
        )),
        E::PublishingDevice { device_name, .. } => ApiError::Conflict(
            crate::account::client::publishing_device_msg(device_name.as_deref()),
        ),
        E::NotPublishingDevice { device_name, .. } => ApiError::Conflict(
            crate::account::client::not_publishing_device_msg(device_name.as_deref()),
        ),
        E::Network(m) => ApiError::Internal(format!("Hub request failed: {m}")),
    }
}

// ── Manifest delta (collab v3 wave 2, Task 8; P9) ────────────────────────────
//
// The hub keeps one `version` per project, bumped by every change a device
// must see (a manifest row, membership, caps, thresholds, dictionary). The
// live feed (`api::collab_live::feed`) pulls a paged manifest delta from the
// cached cursor whenever an event says the project moved.

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
    pub(crate) fn as_str(self) -> &'static str {
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
/// sync applied for one project. Emitted once per non-zero kind.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CollabFramesChange {
    pub project_id: String,
    pub kind: FramesChangeKind,
    pub count: usize,
}

/// The devices whose frames are "own" on THIS device (amendment A6): this
/// device's key, plus every device it REPLACED (fix round 1 — the verified
/// device replace inherits the old device's files and its authorship),
/// the latter only for this device's account.
#[derive(Debug, Clone, Default)]
pub(crate) struct OwnDevices {
    /// This device's base64 public key (`api::account::own_device_id`).
    pub me: String,
    /// Replaced device id → the account it was replaced in (`None` when
    /// unknown then).
    pub replaced: std::collections::HashMap<String, Option<String>>,
    /// This device's account (in the project at hand, from its membership
    /// snapshot, else the live session's), when known.
    pub account: Option<String>,
}

impl OwnDevices {
    /// Only this device — no replaced devices (tests, callers without a
    /// catalog).
    #[cfg(test)]
    pub(crate) fn only(me: &str) -> Self {
        OwnDevices {
            me: me.to_string(),
            ..Default::default()
        }
    }

    /// Whether a frame announced by `device` under `publisher_account` is
    /// this device's own. A replaced device counts only for the account it
    /// was replaced in and only when that is this device's account — a
    /// replaced id never makes another account's frame own.
    pub(crate) fn is_mine(&self, device: &str, publisher_account: &str) -> bool {
        if device == self.me {
            return true;
        }
        let Some(recorded) = self.replaced.get(device) else {
            return false;
        };
        let recorded_ok = recorded.as_deref().is_none_or(|a| a == publisher_account);
        match (&self.account, recorded) {
            (Some(mine), _) => mine == publisher_account && recorded_ok,
            (None, Some(a)) => a == publisher_account,
            (None, None) => false,
        }
    }
}

/// Amendment A6: a frame is `own` on THIS device only when this device
/// published it — `publisherDeviceId` is one of [`OwnDevices`] (this
/// device's key, or a device it replaced, for its own account). The hub's
/// account-level `own` is overwritten here, before anything classifies or
/// stores the row.
///
/// A row with no recorded device (`publisherDeviceId: null`): a frame this
/// device already holds as own — an `own` row with a local path — stays own;
/// any other is a replica. Frames another device of this account published
/// are therefore replicas here: fetched under this device's policy, held,
/// served and claimed (spec §10 "two devices of one account → one publisher,
/// two holder rows"). The data role still rules receiving: a `send`-role
/// member's devices never receive anything, including their own account's
/// frames from another device — the role means "contributes, stores
/// nothing" ([`role_allows_replication`]; fix round 2, M7).
pub(crate) fn derive_device_own(
    v: &mut crate::collab::hub_client::FrameViewWire,
    own: &OwnDevices,
    prev: Option<&LocalFrameRow>,
) {
    v.own = match v.publisher_device_id.as_deref() {
        Some(device) => own.is_mine(device, &v.publisher_account_id),
        None => prev.is_some_and(|p| p.origin == FrameOrigin::Own && p.landed_path.is_some()),
    };
}

/// This device's account as the catalog knows it: the member of a cached
/// project snapshot whose nodes list this device, else the live session's
/// `hello.accountId`.
fn my_account_id(
    conn: &rusqlite::Connection,
    me: &str,
    project: Option<&crate::db::collab::CollabProjectRow>,
) -> Result<Option<String>, ApiError> {
    let in_snapshot = |members_json: &str| -> Option<String> {
        serde_json::from_str::<Vec<SnapshotMember>>(members_json)
            .ok()?
            .into_iter()
            .find(|m| m.nodes.iter().any(|n| n == me))
            .map(|m| m.account_id)
    };
    if let Some(a) = project.and_then(|p| in_snapshot(&p.members_json)) {
        return Ok(Some(a));
    }
    if let Some(a) =
        crate::db::collab_live::meta_get(conn, crate::db::collab_live::META_ACCOUNT_ID)?
    {
        return Ok(Some(a));
    }
    if project.is_none() {
        for p in crate::db::collab::list_projects(conn)? {
            if let Some(a) = in_snapshot(&p.members_json) {
                return Ok(Some(a));
            }
        }
    }
    Ok(None)
}

/// [`OwnDevices`] for a manifest apply of `project`, or the error that stops
/// it: never guess — an apply without this device's key would turn every
/// own frame into a replica.
pub(crate) fn own_devices(
    ctx: &ServiceContext,
    conn: &rusqlite::Connection,
    project: &crate::db::collab::CollabProjectRow,
) -> Result<OwnDevices, ApiError> {
    let me = crate::api::account::own_device_id(ctx).map_err(|e| {
        tracing::error!(project_id = %project.project_id, error = %e, "manifest apply: this device's key is unavailable");
        e
    })?;
    let replaced = crate::db::collab_live::replaced_devices(conn)?;
    let account = if replaced.is_empty() {
        None
    } else {
        my_account_id(conn, &me, Some(project))?
    };
    Ok(OwnDevices {
        me,
        replaced,
        account,
    })
}

/// The member of a signature-verified cached membership snapshot whose
/// nodes list `device` (A6: the account a device replace records), from the
/// live projects only (a project lost to this sign-in is not consulted).
#[cfg(all(feature = "render", feature = "solver"))]
pub(crate) fn snapshot_account_listing(
    conn: &rusqlite::Connection,
    device: &str,
) -> Result<Option<String>, ApiError> {
    for p in crate::db::collab::list_projects(conn)? {
        let members: Vec<SnapshotMember> = match serde_json::from_str(&p.members_json) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(project_id = %p.project_id, error = %e, "cached members_json does not parse; skipped");
                continue;
            }
        };
        if let Some(m) = members
            .into_iter()
            .find(|m| m.nodes.iter().any(|n| n == device))
        {
            return Ok(Some(m.account_id));
        }
    }
    Ok(None)
}

/// Fix rounds 1+2 (A6): the verified device replace — this device now
/// stands in for `old_device` (its files, its authorship) in `account_id`,
/// the account the replace flow verified. One upsert; its failure is the
/// caller's error. [`rederive_own_frames`] then applies it to the cache.
#[cfg(all(feature = "render", feature = "solver"))]
pub(crate) fn record_device_replaced(
    ctx: &ServiceContext,
    old_device: &str,
    account_id: &str,
) -> Result<(), ApiError> {
    let database = db(ctx)?;
    let conn = database.conn();
    crate::db::collab_live::record_replaced_device(&conn, old_device, Some(account_id))
        .map_err(|e| {
            tracing::error!(device_id = %old_device, error = %format!("{e:#}"), "recording the replaced device failed");
            ApiError::from(e)
        })?;
    tracing::info!(device_id = %old_device, account_id, "replaced device recorded: its frames are own here");
    Ok(())
}

#[cfg(test)]
thread_local! {
    /// Test seam (A6 fix round 2): the next [`rederive_own_frames`] fails
    /// this project's pass once.
    pub(crate) static FAIL_REDERIVE_ONCE: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// Re-derive `own` for every cached frame of every live project from its
/// stored manifest row (A6: after a device replace, the frames the replaced
/// device published become own here at once; the storage walk that follows
/// re-adopts their files as own). Idempotent and BEST-EFFORT (fix round 2):
/// a project whose pass fails is logged and skipped — the replace it
/// follows has already retired the old device, so it must not fail on it;
/// a retry of the replace (or any later manifest apply) re-derives it.
/// Returns the rows that flipped.
#[cfg(all(feature = "render", feature = "solver"))]
pub(crate) fn rederive_own_frames(ctx: &ServiceContext) -> usize {
    use crate::db::collab_frames as frames_db;
    let pass = || -> Result<usize, ApiError> {
        let database = db(ctx)?;
        let conn = database.conn();
        let mut flipped = 0usize;
        for project in crate::db::collab::list_projects(&conn)? {
            let pid = project.project_id.clone();
            let one = || -> Result<usize, ApiError> {
                #[cfg(test)]
                if FAIL_REDERIVE_ONCE.with(|f| {
                    let mut f = f.borrow_mut();
                    if f.as_deref() == Some(pid.as_str()) {
                        *f = None;
                        true
                    } else {
                        false
                    }
                }) {
                    return Err(ApiError::Internal("injected re-derive failure".into()));
                }
                let own = own_devices(ctx, &conn, &project)?;
                let mut routes = frames_db::EngineRoutes::default();
                // IMMEDIATE: reads every row, then writes the ones that flip.
                let tx = rusqlite::Transaction::new_unchecked(
                    &conn,
                    rusqlite::TransactionBehavior::Immediate,
                )?;
                let mut n = 0usize;
                for row in frames_db::list_for_project(&tx, &pid)? {
                    let Some(mut v) =
                        parse_manifest_wire(&pid, &row.frame_uuid, &row.manifest_json, "replace")
                    else {
                        continue;
                    };
                    derive_device_own(&mut v, &own, Some(&row));
                    if v.own != (row.origin == FrameOrigin::Own) {
                        frames_db::upsert_from_manifest_deferred(&tx, &pid, &v, &mut routes)?;
                        n += 1;
                    }
                }
                tx.commit()?;
                routes.route();
                Ok(n)
            };
            match one() {
                Ok(n) => flipped += n,
                Err(e) => {
                    tracing::error!(project_id = %pid, error = %e, "re-deriving own frames failed for this project; a retry or the next manifest apply repairs it");
                }
            }
        }
        Ok(flipped)
    };
    match pass() {
        Ok(n) => {
            tracing::info!(count = n, "own frames re-derived");
            n
        }
        Err(e) => {
            tracing::error!(error = %e, "re-deriving own frames failed; a retry or the next manifest apply repairs it");
            0
        }
    }
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
    let result = sync_manifest_inner(ctx, project_id, emitter, vouched_version, false).await;
    if let Err(e) = &result {
        tracing::warn!(
            project_id,
            error = %e,
            "manifest sync failed; the next sync resumes from the stored cursor"
        );
    }
    result.map(|(changes, _seen, _project_version)| changes)
}

/// Refetch the whole manifest from 0 and prune rows the hub no longer lists
/// (own rows are never pruned, `delete_not_in`). Returns every uuid the hub
/// listed (the epoch path compares it with the own rows, plan P25) and the
/// manifest's own freshly-fetched `projectVersion` — fix round 2, item 6:
/// a caller's cursor fallback must read THIS value, never a pre-update DB
/// row, which can itself be stale (the same bug class as C1, just reached
/// through a caller-supplied fallback instead of `max_mv`).
#[cfg(all(feature = "render", feature = "solver"))]
pub(crate) async fn sync_manifest_full(
    ctx: &ServiceContext,
    project_id: &str,
    emitter: Option<&dyn ProgressEmitter>,
    vouched_version: Option<i64>,
) -> Result<(HashSet<String>, i64), ApiError> {
    let (_changes, seen, project_version) =
        sync_manifest_inner(ctx, project_id, emitter, vouched_version, true).await?;
    Ok((seen, project_version))
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

/// The shared body of [`sync_manifest`]/[`sync_manifest_full`]: pull a
/// project's manifest delta (or the whole manifest from 0 when `force_full`,
/// the wave-3 epoch-reload path) into `project_frames_local`. Returns the
/// per-kind changes, every uuid the hub listed this call (used by the epoch
/// path to compare against the own rows still cached, plan P25), and the
/// manifest's own `projectVersion` as of the last page fetched — the
/// authoritative cursor value for a caller that must not trust an event's
/// head beyond what the manifest actually confirmed (T5 ruling).
pub(crate) async fn sync_manifest_inner(
    ctx: &ServiceContext,
    project_id: &str,
    emitter: Option<&dyn ProgressEmitter>,
    vouched_version: Option<i64>,
    force_full: bool,
) -> Result<(Vec<CollabFramesChange>, HashSet<String>, i64), ApiError> {
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
    let own = {
        let database = db(ctx)?;
        let conn = database.conn();
        own_devices(ctx, &conn, &project)?
    };

    let caps_changed = project.gov_caps_json != project.synced_caps_json;
    let full = caps_changed || force_full;
    let start = if full { 0 } else { project.manifest_cursor };
    let mut counts: std::collections::BTreeMap<FramesChangeKind, usize> =
        std::collections::BTreeMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    // A full fetch (caps change or force_full — the epoch-reload path) must
    // never seed this from the OLD cursor (C1 fix round): after a hub
    // restore, fresh rows can carry a manifestVersion BELOW the stale
    // pre-restore cursor. Seeding from it would store that stale-high value
    // right back as the "highest applied" cursor, and a later incremental
    // fetch (`since = manifest_cursor`) would silently skip every such row
    // forever. A full fetch's cursor is exactly the highest version IT saw.
    let mut max_mv = if full { 0 } else { project.manifest_cursor };
    let mut project_version = project.hub_version;
    let mut applied = 0usize;

    let fetched: Result<(), ApiError> = async {
        let mut since = start;
        let mut after: Option<String> = None;
        loop {
            let mut page = client
                .manifest_page(
                    &token,
                    project_id,
                    since,
                    after.as_deref(),
                    MANIFEST_PAGE_LIMIT,
                )
                .await
                .map_err(client_err)?;
            project_version = page.project_version;
            {
                let database = db(ctx)?;
                let conn = database.conn();
                // IMMEDIATE: read-then-write — see `FeedApplier::apply_inline`
                // (a deferred upgrade under another writer fails at once and
                // the live event that asked for this page is dropped).
                let tx = rusqlite::Transaction::new_unchecked(
                    &conn,
                    rusqlite::TransactionBehavior::Immediate,
                )?;
                let mut routes = frames_db::EngineRoutes::default();
                for v in page.rows.iter_mut() {
                    let prev = frames_db::get(&tx, project_id, &v.frame_uuid)?;
                    derive_device_own(v, &own, prev.as_ref());
                    for kind in classify_frame_change(prev.as_ref(), v) {
                        *counts.entry(kind).or_default() += 1;
                    }
                    frames_db::upsert_from_manifest_deferred(&tx, project_id, v, &mut routes)?;
                    seen.insert(v.frame_uuid.clone());
                    max_mv = max_mv.max(v.manifest_version);
                }
                tx.commit()?;
                routes.route();
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
        let pruned = if full {
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
        force_full,
        hub_version = ?vouched_version,
        manifest_cursor = max_mv,
        "manifest synced"
    );
    Ok((changes, seen, project_version))
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

/// The night a frame belongs to when all we have is its `DATE-OBS` (another
/// member's frame): the UTC date of `dateObs − 12 h` (spec 2026-09-29 §5.2).
pub(crate) fn night_of_date_obs(date_obs: &str) -> Option<String> {
    use chrono::{DateTime, NaiveDateTime, Utc};
    let t: DateTime<Utc> = DateTime::parse_from_rfc3339(date_obs)
        .map(|d| d.with_timezone(&Utc))
        .or_else(|_| {
            NaiveDateTime::parse_from_str(date_obs, "%Y-%m-%dT%H:%M:%S%.f").map(|n| n.and_utc())
        })
        .ok()?;
    Some(
        (t - chrono::Duration::hours(12))
            .format("%Y-%m-%d")
            .to_string(),
    )
}

/// One cached per-frame manifest row of a project (mine or a peer's),
/// projected for the frames list (wave 2 Task 11; wave 3 Task 16: the local
/// state and the live holder counts replace the retired on-disk / GC /
/// declined flags and the hub's holder count).
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ProjectFrameView {
    pub frame_uuid: String,
    pub file_name: String,
    pub publisher: String,
    /// The publisher's hub account id (Task 6 — Library table Publisher
    /// grouping, distinct from `publisher`, the display name).
    pub publisher_account_id: String,
    /// I published this frame.
    pub own: bool,
    pub filter: String,
    pub exptime_sec: f64,
    pub date_obs: Option<String>,
    /// Hub-mirrored state: `pending` | `published` | `rejected`.
    pub state: String,
    pub accepted: bool,
    pub accepted_reason: Option<String>,
    /// This device's state for the frame (spec §9).
    pub local_state: crate::api::collab_live::LocalStateView,
    pub on_disk: bool,
    /// Other member devices holding the current version that are online and
    /// serving now (0 while the live exchange is off).
    pub holders_online: usize,
    /// Other member devices holding the current version, offline included.
    pub holders_total: usize,
    /// L7: the current version is held only by the publisher's devices, and
    /// none of them is online.
    pub waiting_for_publisher: bool,
    /// A changed (quarantined) replica whose frame has a newer version than
    /// the one it was quarantined at.
    pub new_version_waiting: bool,
    pub byte_size: i64,
    pub content_version: i32,
    pub last_error: Option<String>,
    /// Parsed from the manifest row's `meta.fwhmArcsec` (`build_frame_meta`).
    pub fwhm_arcsec: Option<f64>,
    /// Parsed from `meta.eccentricity`.
    pub eccentricity: Option<f64>,
    /// Parsed from `meta.starsDetected`.
    pub stars_detected: Option<i64>,
    /// Parsed from `meta.instrume`.
    pub camera: Option<String>,
    /// Parsed from `meta.telescope`.
    pub telescope: Option<String>,
    /// Parsed from `meta.medianSnr`.
    pub median_snr: Option<f64>,
    /// The UTC date of `dateObs − 12 h` ([`night_of_date_obs`]) — there is no
    /// catalog `imaging_nights` row for another member's frame, so this is
    /// the best a cached manifest row can do.
    pub night: Option<String>,
    /// Own rows only — a [`crate::collab::contributor_state::ContributorState`]
    /// key, the same derivation the frame set's Project block uses (Task 7,
    /// spec §8.1). Filled by `api::collab_live::surface::list_collab_frames`;
    /// `None` for a replica row, or when this cache-only builder ran without
    /// it (`list_project_frames`). Also `None` for an own row NOT YET
    /// ADOPTED (`source_frame_id IS NULL` — a manifest-delivered row
    /// `db::collab_frames::adopt_own` has not yet bound to a local frame;
    /// `adopt_own` is what SETS `source_frame_id`, so a null one is the
    /// pre-adoption state, never the post-adoption one): `own_contributor_states`
    /// keys its own-row map by `source_frame_id`, so a row with no local
    /// frame bound yet has no local frame to hang a chip off of — it never
    /// gets one, by construction, not by an explicit "no chip" branch.
    pub contributor_state: Option<String>,
    pub contributor_reason: Option<String>,
}

/// What the live exchange knows about a frame beyond its catalog row: other
/// holders of its current version and whether it waits for its publisher
/// (Task 16; all zero / `false` while no live exchange runs).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameLiveInfo {
    pub holders_online: usize,
    pub holders_total: usize,
    pub waiting_for_publisher: bool,
}

impl ProjectFrameView {
    /// Combines the reliable local columns (state/accepted/local state/… —
    /// kept current by the manifest sync and the live exchange) with the
    /// fields only the retained manifest row carries
    /// (exptime/dateObs/acceptedReason/meta metrics) and the live holder
    /// counts. A row whose `manifest_json` fails to parse (it never should —
    /// this cache only ever writes it via `serde_json::to_string` of a
    /// decoded [`FrameViewWire`]) still returns a view, with those fields
    /// empty and a `warn!` — never a lost frame from the list.
    fn from_local_row(row: LocalFrameRow, live: FrameLiveInfo, new_version_waiting: bool) -> Self {
        let wire = parse_manifest_wire(
            &row.project_id,
            &row.frame_uuid,
            &row.manifest_json,
            "list_project_frames",
        );
        let (
            exptime_sec,
            date_obs,
            accepted_reason,
            fwhm_arcsec,
            eccentricity,
            stars_detected,
            camera,
            telescope,
            median_snr,
            night,
        ) = match &wire {
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
                w.meta
                    .get("instrume")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                w.meta
                    .get("telescope")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                w.meta.get("medianSnr").and_then(serde_json::Value::as_f64),
                w.date_obs.as_deref().and_then(night_of_date_obs),
            ),
            None => (0.0, None, None, None, None, None, None, None, None, None),
        };
        ProjectFrameView {
            frame_uuid: row.frame_uuid,
            file_name: row.file_name,
            publisher: row.publisher_display,
            publisher_account_id: row.publisher_account_id.clone(),
            own: row.origin == FrameOrigin::Own,
            filter: row.filter_canonical,
            exptime_sec,
            date_obs,
            state: row.state,
            accepted: row.accepted,
            accepted_reason,
            local_state: row.local_state.into(),
            on_disk: row.on_disk,
            holders_online: live.holders_online,
            holders_total: live.holders_total,
            waiting_for_publisher: live.waiting_for_publisher,
            new_version_waiting,
            byte_size: row.byte_size,
            content_version: row.content_version,
            last_error: row.last_error,
            fwhm_arcsec,
            eccentricity,
            stars_detected,
            camera,
            telescope,
            median_snr,
            night,
            contributor_state: None,
            contributor_reason: None,
        }
    }
}

/// Every cached frame of a project (cache-only — no hub call), ordered by
/// frame uuid (the `list_for_project` order), without live holder counts.
/// The manifest sync and the live exchange keep the cache current; this
/// never fetches. The command surface adds the holder counts
/// (`collab_live::surface::list_collab_frames`).
pub fn list_project_frames(
    ctx: &ServiceContext,
    project_id: &str,
) -> Result<Vec<ProjectFrameView>, ApiError> {
    list_project_frames_with(ctx, project_id, |_| FrameLiveInfo::default())
}

/// [`list_project_frames`] with each row's live holder information from
/// `live` (called once per row, with the catalog connection released).
pub fn list_project_frames_with(
    ctx: &ServiceContext,
    project_id: &str,
    live: impl Fn(&LocalFrameRow) -> FrameLiveInfo,
) -> Result<Vec<ProjectFrameView>, ApiError> {
    let (rows, quarantined) = {
        let db = db(ctx)?;
        let conn = db.conn();
        let rows = crate::db::collab_frames::list_for_project(&conn, project_id)?;
        let quarantined: HashMap<String, i32> =
            crate::db::collab_live::list_quarantine(&conn, project_id)?
                .into_iter()
                .map(|q| (q.frame_uuid, q.quarantined_version))
                .collect();
        (rows, quarantined)
    };
    Ok(rows
        .into_iter()
        .map(|row| {
            let info = live(&row);
            let new_version_waiting = row.local_state
                == crate::db::collab_frames::LocalState::Quarantined
                && quarantined
                    .get(&row.frame_uuid)
                    .is_some_and(|v| row.content_version > *v);
            ProjectFrameView::from_local_row(row, info, new_version_waiting)
        })
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
#[cfg(all(feature = "render", feature = "solver"))]
pub(crate) const COLLABORATION_ROOT_REQUIRED: &str =
    "set a Collaboration folder in File Manager → Folders first";

/// The configured Collaboration root — required for any collab receive or
/// publish (P25). Absent ⇒ `ApiError::Invalid` carrying
/// [`COLLABORATION_ROOT_REQUIRED`], logged at `warn!`.
#[cfg(all(feature = "render", feature = "solver"))]
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
        if let Some(dir) = crate::db::collab_frames::publisher_dir(
            conn,
            &project.project_id,
            account_id,
            collab_root,
        )? {
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

/// Emitted at most once per second per project while the live exchange
/// lands frames ([`CollabFramesLanded`]) — a burst outcome, never progress.
pub const COLLAB_FRAMES_LANDED_EVENT: &str = "collab-frames-landed";

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

/// Payload of [`COLLAB_FRAMES_LANDED_EVENT`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CollabFramesLanded {
    pub project_id: String,
    pub landed: usize,
    pub failed: usize,
    pub awaiting_gc: usize,
}

/// `"size:mtime_secs"` — the same spelling publish records.
pub(crate) fn size_mtime_from(meta: &std::fs::Metadata) -> String {
    let secs = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{}:{secs}", meta.len())
}

/// A string field of a row's verbatim manifest JSON.
pub(crate) fn manifest_str(row: &LocalFrameRow, key: &str) -> Option<String> {
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
pub(crate) fn policy_matches(row: &LocalFrameRow, policy: &ReplicationPolicy) -> bool {
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

/// Is `row` a peer frame this device may hold at all (before policy)? A
/// frame the user stopped keeping is not (L6: until "Keep again").
fn replicable(row: &LocalFrameRow) -> bool {
    row.state == "published"
        && row.accepted
        && row.origin == FrameOrigin::Replica
        && row.local_state != crate::db::collab_frames::LocalState::NotKept
}

/// The node bound on the context, if any (never binds one).
#[cfg(all(feature = "render", feature = "solver"))]
pub(crate) async fn bound_node(
    ctx: &ServiceContext,
) -> Option<Arc<crate::sharing::iroh::node::SharedIrohNode>> {
    ctx.iroh_node.lock().await.clone()
}

/// Full-file xxh3 on a blocking thread.
pub(crate) async fn xxh3_on_blocking(path: &Path) -> Result<String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || crate::package::xxh3_full_file(&path))
        .await
        .context("xxh3 task join")?
}

/// The per-project lock between the storage engine's per-frame checks on
/// one side and a landing on the other (wave-2 ruling R18). Each holds it
/// per frame and nobody holds it across a network fetch. Keyed by catalog +
/// project. The publish run holds it too, per frame, across rename → seed →
/// own-row write (final review I2), so no check sees a regenerated file its
/// row does not describe yet.
pub(crate) fn project_disk_lock(
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
#[cfg(all(feature = "render", feature = "solver"))]
fn collaboration_root_quiet(ctx: &ServiceContext) -> Option<PathBuf> {
    let db = match db(ctx) {
        Ok(db) => db,
        Err(e) => {
            tracing::warn!(error = %e, "read collaboration root failed: catalog unavailable");
            return None;
        }
    };
    let conn = db.conn();
    match crate::db::scan_root_path_of_kind(&conn, "collaboration") {
        Ok(p) => p.map(PathBuf::from),
        Err(e) => {
            tracing::warn!(error = %e, "read collaboration root failed");
            None
        }
    }
}

/// Verify the Collaboration root's storage marker (spec §9.1, plan P22)
/// against what this device last recorded: refuse another device's disk,
/// forget a stale record for a path that changed, and persist a fresh or
/// returning marker. Never mounts or unmounts anything — the caller gates
/// `set_collab_root`/holds/fetch/publish on the returned state's
/// `serving()`/`fetching()`. This is the ONE check both
/// `scan_roots::set_collaboration_dir`'s designation and this module's own
/// lazy mount ([`ensure_collab_store`]) run, so a folder that fails to
/// designate can never later be mounted, and vice versa.
/// The blocking half of [`check_storage_marker`]: `check_store` plus the
/// write-on-`Adopt` step, entirely off the async runtime (fix round 1 —
/// this used to run on the connection-holding async path). Returns the
/// resolved state and, when a marker was freshly written OR recognized as
/// already naming `me`, the marker to record. The adoption itself is
/// [`crate::collab::storage::marker::check_and_adopt`], serialized across
/// every adopter of the root (the designation, the lazy mount, the mount at
/// bind and a live session's guard) so they all adopt ONE store id.
fn check_and_adopt_marker(
    root: &Path,
    recorded: Option<crate::collab::storage::marker::StoreMarker>,
    me: &str,
) -> (
    crate::collab::storage::marker::StoreState,
    Option<crate::collab::storage::marker::StoreMarker>,
) {
    crate::collab::storage::marker::check_and_adopt(root, recorded.as_ref(), me)
}

/// The result of [`check_storage_marker`]: the resolved state, and — when a
/// marker was freshly written or recognized as already naming this device —
/// the marker a caller should [`commit_marker_check`] once whatever it
/// gates (a mount) actually succeeds. Never persisted by the check itself
/// (fix round 2, ruling 6): a caller that never mounts, or whose mount
/// fails, must leave any previously recorded marker exactly as it was.
pub(crate) struct MarkerCheck {
    pub state: crate::collab::storage::marker::StoreState,
    pub pending: Option<crate::collab::storage::marker::StoreMarker>,
}

/// Verify the Collaboration root's storage marker (spec §9.1, plan P22)
/// against what this device last recorded: refuse another device's disk,
/// treat a recorded marker for a DIFFERENT path as not recorded for this
/// check (comparing it against this path's on-disk marker would compare
/// apples to oranges). Never mounts, unmounts or writes to the catalog —
/// the caller gates `set_collab_root`/holds/fetch/publish on the returned
/// state's `serving()`/`fetching()`, and persists `pending` (via
/// [`commit_marker_check`]) only once its own mount actually succeeds. This
/// is the ONE check `scan_roots::set_collaboration_dir`'s designation, this
/// module's own lazy mount ([`ensure_collab_store`]), and `api::sync`'s
/// mount-at-bind all run, so a folder that fails to designate can never
/// later be mounted, and vice versa.
///
/// Fix round 2, ruling 1: makes NO hub call and offers no automatic
/// takeover of any kind — absence from this account's active device list is
/// not proof of same-account ownership (it could be another account's
/// device, a plain-revoked device, or a swapped disk). A marker naming
/// another device always refuses; classifying that refusal (a still-active
/// device of this account vs. one this device cannot vouch for at all) is
/// the caller's job at the ONE place that surfaces it to the user
/// (`scan_roots::check_storage_marker_for_designation`), never here — this
/// function runs on background paths (lazy mount, bind-mount) too, where a
/// hub call's failure-side-effect (a 401 clearing the local session) has no
/// business firing.
pub(crate) async fn check_storage_marker(
    ctx: &ServiceContext,
    root: &Path,
) -> Result<MarkerCheck, ApiError> {
    let me = crate::api::account::own_device_id(ctx)?;
    let root_str = root.to_string_lossy().to_string();

    // A recorded marker for a DIFFERENT path is simply not `recorded` for
    // THIS check — never deleted here (ruling 6: only a caller's successful
    // `commit_marker_check` ever overwrites the persisted record).
    let recorded = {
        let db = db(ctx)?;
        let conn = db.conn();
        let stored_path = crate::db::collab_live::store_marker_path(&conn)?;
        if stored_path.as_deref() == Some(root_str.as_str()) {
            crate::db::collab_live::recorded_store_marker(&conn)?
        } else {
            None
        }
    }; // the connection is dropped here, before the filesystem I/O below.

    let root_for_task = root.to_path_buf();
    let me_for_task = me.clone();
    let (state, pending) = tokio::task::spawn_blocking(move || {
        check_and_adopt_marker(&root_for_task, recorded, &me_for_task)
    })
    .await
    .map_err(|e| ApiError::Internal(format!("storage marker check task join: {e}")))?;

    Ok(MarkerCheck { state, pending })
}

/// Persist a [`MarkerCheck`]'s `pending` marker — called ONLY once whatever
/// the check gated (a mount) has actually succeeded (fix round 2, ruling
/// 6). A no-op when `pending` is `None` (nothing new to record — the
/// existing record, if any, is left exactly as it was).
pub(crate) fn commit_marker_check(
    ctx: &ServiceContext,
    root: &Path,
    pending: Option<crate::collab::storage::marker::StoreMarker>,
) -> Result<(), ApiError> {
    let Some(m) = pending else {
        return Ok(());
    };
    let db = db(ctx)?;
    let conn = db.conn();
    crate::db::collab_live::record_store_marker(&conn, &m, &root.to_string_lossy())?;
    Ok(())
}

/// How long a failed lazy mount waits before the next attempt (I3).
#[cfg(all(feature = "render", feature = "solver"))]
const COLLAB_MOUNT_RETRY: std::time::Duration = std::time::Duration::from_secs(60);

/// Per catalog: when the last lazy mount attempt ran, and whether its failure
/// was already logged at `warn!` (the latch — later failures log at `debug!`
/// until a mount succeeds).
#[cfg(all(feature = "render", feature = "solver"))]
static COLLAB_MOUNT_ATTEMPTS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, bool)>>,
> = std::sync::OnceLock::new();

/// Mark `key`'s latch as already warned (a later failure of the same shape
/// logs at `debug!` instead of `warn!`, until a mount succeeds and the entry
/// is dropped).
#[cfg(all(feature = "render", feature = "solver"))]
fn latch_mount_warned(key: &str) {
    if let Some(attempts) = COLLAB_MOUNT_ATTEMPTS.get() {
        if let Some(entry) = attempts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(key)
        {
            entry.1 = true;
        }
    }
}

/// The collab store of the bound node, mounting it lazily when the
/// Collaboration root exists but the store is not mounted (final review I3):
/// a root that appeared after startup (a volume plugged in late) or whose
/// mount at bind failed is otherwise never retried. Rate-limited to one
/// attempt per [`COLLAB_MOUNT_RETRY`] per catalog; the first failure logs a
/// `warn!`, later ones `debug!`, until a mount succeeds. Never binds a node.
/// `None` = no node, no root, or still unmounted — the caller must then
/// neither report holds nor fetch nor publish (the device cannot serve).
#[cfg(all(feature = "render", feature = "solver"))]
pub(crate) async fn ensure_collab_store(ctx: &ServiceContext) -> Option<iroh_blobs::api::Store> {
    let node = bound_node(ctx).await?;
    if let Some(store) = node.collab_store() {
        return Some(store);
    }
    let root = collaboration_root_quiet(ctx)?;
    if !root.is_dir() {
        return None;
    }
    let key = match db(ctx) {
        Ok(db) => db.path().display().to_string(),
        Err(e) => {
            tracing::warn!(error = %e, "lazy collab store mount skipped: catalog unavailable");
            return None;
        }
    };
    let already_warned = {
        let mut attempts = COLLAB_MOUNT_ATTEMPTS
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match attempts.get(&key) {
            Some((at, _)) if at.elapsed() < COLLAB_MOUNT_RETRY => return None,
            Some((_, warned)) => {
                let warned = *warned;
                attempts.insert(key.clone(), (std::time::Instant::now(), warned));
                warned
            }
            None => {
                attempts.insert(key.clone(), (std::time::Instant::now(), false));
                false
            }
        }
    };
    let check = match check_storage_marker(ctx, &root).await {
        Ok(check) => check,
        Err(e) => {
            if already_warned {
                tracing::debug!(path = %root.display(), error = %format!("{e:#}"), "lazy collab store mount skipped again: storage marker check failed");
            } else {
                tracing::warn!(path = %root.display(), error = %format!("{e:#}"), "lazy collab store mount skipped: storage marker check failed");
                latch_mount_warned(&key);
            }
            return None;
        }
    };
    if !check.state.serving() {
        if already_warned {
            tracing::debug!(path = %root.display(), state = ?check.state, "lazy collab store mount skipped again: storage not available");
        } else {
            tracing::warn!(path = %root.display(), state = ?check.state, "lazy collab store mount skipped: storage not available");
            latch_mount_warned(&key);
        }
        return None;
    }
    match node.set_collab_root(Some(&root)).await {
        Ok(()) => {
            if let Err(e) = commit_marker_check(ctx, &root, check.pending) {
                tracing::error!(path = %root.display(), error = %e, "recording the storage marker after a lazy mount failed");
            }
            if let Some(attempts) = COLLAB_MOUNT_ATTEMPTS.get() {
                attempts
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .remove(&key);
            }
            tracing::info!(path = %root.display(), "collab store mounted lazily");
            node.collab_store()
        }
        Err(e) => {
            if already_warned {
                tracing::debug!(path = %root.display(), error = %format!("{e:#}"), "lazy collab store mount failed again");
            } else {
                tracing::warn!(path = %root.display(), error = %format!("{e:#}"), "lazy collab store mount failed; holds, fetch and publish wait for it");
                if let Some(attempts) = COLLAB_MOUNT_ATTEMPTS.get() {
                    if let Some(entry) = attempts
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .get_mut(&key)
                    {
                        entry.1 = true;
                    }
                }
            }
            None
        }
    }
}

/// A replica's landed path must lie under the current Collaboration root;
/// one outside it (a root that moved) counts as missing (m5). Own frames may
/// live anywhere (P26).
pub(crate) fn inside_root(root: Option<&Path>, row: &LocalFrameRow, path: &Path) -> bool {
    row.origin == FrameOrigin::Own || root.is_none_or(|r| path.starts_with(r))
}

/// Record a frame-level failure on its row (logged by the caller).
pub(crate) fn record_frame_error(
    ctx: &ServiceContext,
    project_id: &str,
    frame_uuid: &str,
    error: &str,
) {
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

/// Delete one tag, logging a failure (the store's open sweep reclaims a
/// stale in-flight tag).
pub(crate) async fn drop_tag(store: &iroh_blobs::api::Store, tag: &str) {
    if let Err(e) = store.tags().delete(tag).await {
        tracing::warn!(tag, error = %e, "delete tag failed");
    }
}

// ── Replication policy ─────────────────────────────────────────────────────

/// The stored policy of a live project. An unreadable document is logged and
/// read as the default (replicate everything) — the same as never set.
pub(crate) fn read_policy(project: &crate::db::collab::CollabProjectRow) -> ReplicationPolicy {
    serde_json::from_str(&project.policy_json).unwrap_or_else(|e| {
        tracing::warn!(project_id = %project.project_id, error = %e, "replication policy unreadable; replicating everything");
        ReplicationPolicy::default()
    })
}

/// What `policy` selects in the project's cached frames. `to_fetch` counts
/// every matching frame not on disk that the policy makes wanted — `wanted`
/// ones and those it brings back from `idle` or `missing` (a quarantined or
/// awaiting-choice frame waits for the user, never for a policy) — oldest
/// first, inside the byte budget.
fn policy_preview(rows: &[LocalFrameRow], policy: &ReplicationPolicy) -> PolicyPreview {
    use crate::db::collab_frames::LocalState;
    let matching: Vec<&LocalFrameRow> = rows
        .iter()
        .filter(|r| replicable(r) && policy_matches(r, policy))
        .collect();
    let mut candidates: Vec<&LocalFrameRow> = matching
        .iter()
        .copied()
        .filter(|r| {
            !r.on_disk
                && !r.awaiting_gc
                && matches!(
                    r.local_state,
                    LocalState::Wanted | LocalState::Idle | LocalState::Missing
                )
        })
        .collect();
    candidates.sort_by(|a, b| {
        manifest_str(a, "createdAt")
            .unwrap_or_default()
            .cmp(&manifest_str(b, "createdAt").unwrap_or_default())
            .then_with(|| a.frame_uuid.cmp(&b.frame_uuid))
    });
    let mut to_fetch: Vec<&LocalFrameRow> = Vec::new();
    let mut held: i64 = rows
        .iter()
        .filter(|r| r.origin == FrameOrigin::Replica && r.on_disk)
        .map(|r| r.byte_size)
        .sum();
    for r in candidates {
        if policy
            .byte_budget
            .is_some_and(|budget| held.saturating_add(r.byte_size) > budget)
        {
            break;
        }
        held += r.byte_size;
        to_fetch.push(r);
    }
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
/// live exchange re-reads the project's need set at once, so a widened
/// policy starts fetching right away. A narrowed one never deletes anything
/// already held.
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
    // Task 9 (P10): the new scope moves the local frame states — a dropped
    // frame goes idle (file kept, not served), a re-included one held or
    // wanted by stat + hash. `api::collab_live` is render+solver-gated.
    #[cfg(all(feature = "render", feature = "solver"))]
    crate::api::collab_live::storage_task::apply_policy(ctx, project_id).map_err(|e| {
        tracing::error!(project_id, error = %e, "replication policy stored but not applied to the local frame states");
        e
    })?;
    tracing::info!(
        project_id,
        count = preview.to_fetch,
        "replication policy set"
    );
    #[cfg(all(feature = "render", feature = "solver"))]
    crate::api::collab_live::notify_local_change(ctx, project_id);
    Ok(preview)
}

/// May this device replicate a project's frames at all? `coordinator ||
/// data_role == "send_receive"` against the CACHED project row. The hub
/// filters holder rows by the same rule, so a stale cache costs at most one
/// fetch whose holds the hub drops.
///
/// Amendment A6 (fix round 2, M7): this holds for every device of the
/// account — a `send` member's second device receives nothing either, not
/// even the frames its own account's other device published: the role
/// means "contributes, stores nothing".
#[cfg(all(feature = "render", feature = "solver"))]
pub(crate) fn role_allows_replication(data_role: &str, is_coordinator: bool) -> bool {
    is_coordinator || data_role == "send_receive"
}

/// Set one project's auto-replication preference (D3 §3.3). Local-only — the hub
/// never learns of it. The live exchange re-reads the project's need set at
/// once: turning it off cancels the project's fetches in flight (§7.4).
pub fn set_project_auto_replicate(
    ctx: &ServiceContext,
    project_id: &str,
    enabled: bool,
) -> Result<(), ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    // R14: a lost project is not a live one — same check as every other
    // project-scoped command.
    live_project(&conn, project_id)?;
    let updated = crate::db::collab::set_auto_replicate(&conn, project_id, enabled)?;
    if updated == 0 {
        return Err(ApiError::Invalid(format!("unknown project {project_id}")));
    }
    tracing::info!(project_id, enabled, "collab auto-replication toggled");
    // Task 9 (P10): the toggle itself changes no frame state; the call
    // re-derives the scope so a stale state is corrected either way.
    drop(conn);
    #[cfg(all(feature = "render", feature = "solver"))]
    crate::api::collab_live::storage_task::apply_policy(ctx, project_id).map_err(|e| {
        tracing::error!(project_id, error = %e, "auto-replication toggled but the local frame states were not re-derived");
        e
    })?;
    #[cfg(all(feature = "render", feature = "solver"))]
    crate::api::collab_live::notify_local_change(ctx, project_id);
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

/// Test-only fixtures shared with sibling modules (Task 5, wave 3): a
/// minimal file-backed [`ServiceContext`] and the hub-wiring helper, `pub(crate)`
/// so `api::collab_live::feed`'s own tests can build the same rig without a
/// duplicate copy.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// A minimal file-backed-`Database` [`ServiceContext`] (no keychain), copied
    /// from `api::sync` / `api::collab` tests. A tempdir-FILE-backed `Database`
    /// (not `:memory:`) so the pool + the receiver's own `CatalogSyncStore` see
    /// one catalog file.
    pub(crate) fn test_ctx() -> (tempfile::TempDir, ServiceContext) {
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

    /// Point `ctx`'s account hub at `uri` + store `token` as the device's own
    /// (mirrors `api::collab::wire_hub`, which keeps its own copy: that
    /// module's tests also set `SYNC_CACHED_RELAYS`, this one's do not).
    pub(crate) fn wire_hub(ctx: &ServiceContext, uri: &str, token: &str) {
        {
            let conn = db(ctx).unwrap().conn();
            crate::db::set_setting(&conn, crate::settings::keys::ACCOUNT_HUB_URL, uri).unwrap();
        }
        crate::api::account::store_token_for_test(ctx, token).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{test_ctx, wire_hub};
    use super::*;
    use crate::db::collab::{upsert_project, CollabProjectRow};
    use crate::db::collab_frames::LocalState;
    use crate::sharing::types::NodeId;
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;
    use rusqlite::Connection;

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
                feed_epoch: None,
                holder_seq: -1,
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
    /// AND parses `meta.fwhmArcsec`/`meta.eccentricity`/`meta.starsDetected`/
    /// `meta.instrume`/`meta.telescope`/`meta.medianSnr` out of the retained
    /// manifest row (`build_frame_meta`'s camelCase keys) — the metrics the
    /// frames table shows without a second query — and derives `night` from
    /// `dateObs` (Task 6). Scoped to the requested project, like every other
    /// cache-only list view.
    #[test]
    fn list_project_frames_reads_metrics_from_meta() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        let view: crate::collab::hub_client::FrameViewWire = serde_json::from_value(serde_json::json!({
            "frameUuid": "u-1", "publisherAccountId": "acc-alice", "publisherDisplayName": "Alice",
            "own": false, "fileName": "c_u-1.fits", "contentVersion": 1, "blake3": "b".repeat(64),
            "byteSize": 4096, "xxh3": "0123456789abcdef", "filterRaw": "Red", "filterCanonical": "R",
            "channel": "mono", "exptimeSec": 300.0, "dateObs": "2026-09-27T03:10:00",
            "meta": {
                "fwhmArcsec": 2.4, "eccentricity": 0.35, "starsDetected": 512,
                "instrume": "ASI6200MM Pro", "telescope": "RC8", "medianSnr": 41.5
            },
            "gateVersion": 0, "accepted": true, "state": "published", "manifestVersion": 1,
            "createdAt": "2026-07-13T00:00:00Z"
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
        assert_eq!(f.date_obs.as_deref(), Some("2026-09-27T03:10:00"));
        assert_eq!(f.state, "published");
        assert!(f.accepted);
        // v3 wave 3 (Task 16): the hub's holder count is gone; the live
        // holder counts are 0 without a live exchange.
        assert_eq!((f.holders_online, f.holders_total), (0, 0));
        assert!(!f.waiting_for_publisher && !f.new_version_waiting);
        assert_eq!(
            f.local_state,
            crate::api::collab_live::LocalStateView::Wanted
        );
        assert_eq!(f.byte_size, 4096);
        assert_eq!(f.content_version, 1);
        assert_eq!(f.fwhm_arcsec, Some(2.4), "parsed from meta.fwhmArcsec");
        assert_eq!(f.eccentricity, Some(0.35), "parsed from meta.eccentricity");
        assert_eq!(
            f.stars_detected,
            Some(512),
            "parsed from meta.starsDetected"
        );
        assert_eq!(
            f.camera.as_deref(),
            Some("ASI6200MM Pro"),
            "parsed from meta.instrume"
        );
        assert_eq!(
            f.telescope.as_deref(),
            Some("RC8"),
            "parsed from meta.telescope"
        );
        assert_eq!(f.median_snr, Some(41.5), "parsed from meta.medianSnr");
        assert_eq!(
            f.night.as_deref(),
            Some("2026-09-26"),
            "UTC date of dateObs - 12h"
        );
        assert_eq!(f.publisher_account_id, "acc-alice");
    }

    /// The UTC date of `dateObs − 12 h` — the night boundary for a frame we
    /// only know through its cached manifest (Task 6, spec 2026-09-29 §5.2).
    #[test]
    fn night_of_date_obs_shifts_twelve_hours_in_utc() {
        assert_eq!(
            night_of_date_obs("2026-09-27T11:59:59Z").as_deref(),
            Some("2026-09-26")
        );
        assert_eq!(
            night_of_date_obs("2026-09-27T12:00:00Z").as_deref(),
            Some("2026-09-27")
        );
        assert_eq!(
            night_of_date_obs("2026-09-27T03:10:00").as_deref(),
            Some("2026-09-26")
        );
        assert_eq!(
            night_of_date_obs("2026-09-27T03:10:00.250").as_deref(),
            Some("2026-09-26")
        );
        assert_eq!(night_of_date_obs("garbage"), None);
    }

    /// This device's node id for `ctx`'s sync dir — the identity a member's
    /// snapshot entry names.
    fn own_node_for(ctx: &ServiceContext) -> NodeId {
        let identity_dir = crate::api::sync::sync_dirs(ctx).unwrap().identity_dir;
        DeviceKey::load_or_create(&device_key_path(&identity_dir))
            .unwrap()
            .node_id()
    }

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

        // Final review M4 (R14): a lost project is refused like every other
        // project-scoped command, and its stored preference is left alone.
        {
            let conn = db(&ctx).unwrap().conn();
            crate::db::collab::mark_lost(&conn, "p-auto").unwrap();
        }
        match set_project_auto_replicate(&ctx, "p-auto", false) {
            Err(ApiError::NotFound(m)) => assert_eq!(m, "project no longer joined"),
            other => panic!("expected the lost-project refusal, got {other:?}"),
        }
        assert!(toggle(&ctx), "a lost project's preference is untouched");
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
            wire_hub(&ctx, &hub.uri(), "tok");
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
                manifest_version: 0,
                manifest_json: "{}".into(),
                landed_path: Some(landed.into()),
                size_mtime_seen: Some("7:1".into()),
                on_disk: true,
                awaiting_gc: false,
                source_frame_id: Some(42),
                recipe_hash: Some("recipe".into()),
                last_error: None,
                updated_at: String::new(),
                local_state: LocalState::OwnHeld,
                frame_seq: None,
            }
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

        /// C1 fix round: a full fetch (`sync_manifest_full`, the epoch-reload
        /// path) must never inherit the OLD `manifest_cursor` into its own
        /// highest-seen tracker. After a hub restore the fresh manifestVersion
        /// sequence can start again below the stale pre-restore cursor; if the
        /// full fetch's tracker started from that stale value instead of 0, it
        /// would store the SAME stale-high number right back as "highest
        /// applied", and a later incremental fetch (`since` = that cursor)
        /// would silently skip every such row forever.
        #[tokio::test]
        async fn a_full_resync_never_inherits_the_stale_cursor() {
            let fx = fx("send_receive", false, false).await;
            fx.hub.seed_frames(PID, "acc-o", &["f1"], "published");
            crate::api::collab::refresh_projects(&fx.ctx).await.unwrap();
            let low_mv = fx.hub.frame(PID, "f1").unwrap().manifest_version;

            // Simulate the stale-high-cursor state a restore leaves behind.
            let caps = project(&fx.ctx).unwrap().gov_caps_json;
            {
                let conn = db(&fx.ctx).unwrap().conn();
                crate::db::collab::set_sync_state(&conn, PID, None, 999, &caps).unwrap();
            }

            sync_manifest_full(&fx.ctx, PID, None, None).await.unwrap();
            assert_eq!(
                project(&fx.ctx).unwrap().manifest_cursor,
                low_mv,
                "the full fetch's own highest row wins, never the stale pre-restore cursor"
            );

            // A frame whose fresh manifestVersion sits well below the stale
            // 999 must still be picked up by the next incremental sync — not
            // silently skipped forever.
            fx.hub.seed_frames(PID, "acc-o", &["f2"], "published");
            sync_manifest(&fx.ctx, PID, None, None).await.unwrap();
            assert!(row(&fx.ctx, "f2").is_some(), "not silently skipped");
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

        /// Fix round 1: a replaced device's frames are own — for this
        /// device's account only, and a replaced id recorded for one account
        /// never counts for another.
        #[test]
        fn a_replaced_device_counts_as_own_for_its_account_only() {
            let mut own = OwnDevices::only("ME");
            own.replaced.insert("OLD".into(), Some("acc-me".into()));
            own.replaced.insert("UNK".into(), None);
            own.account = Some("acc-me".into());
            assert!(own.is_mine("ME", "acc-me"));
            assert!(own.is_mine("OLD", "acc-me"));
            assert!(!own.is_mine("OLD", "acc-x"), "another account's frame");
            assert!(
                own.is_mine("UNK", "acc-me"),
                "unknown then, this account now"
            );
            assert!(!own.is_mine("UNK", "acc-x"));
            assert!(!own.is_mine("PEER", "acc-me"));
            // This device's account unknown: only a recorded account counts.
            own.account = None;
            assert!(own.is_mine("OLD", "acc-me"));
            assert!(!own.is_mine("OLD", "acc-x"));
            assert!(!own.is_mine("UNK", "acc-me"));
            // Recorded for one account, this device now in another: never.
            own.account = Some("acc-x".into());
            assert!(!own.is_mine("OLD", "acc-x"));
        }

        /// A6: `own` follows the recorded device, never the account; with no
        /// recorded device only a row already held here as own (with a path)
        /// stays own.
        #[test]
        fn own_is_derived_per_device() {
            use crate::collab::hub_client::FrameViewWire;
            let mut v: FrameViewWire = serde_json::from_value(serde_json::json!({
            "frameUuid":"u","publisherAccountId":"acc-me","publisherDisplayName":"Me","own":true,
            "fileName":"u.fits","contentVersion":1,"blake3":"b","byteSize":1,"xxh3":"x",
            "filterRaw":"L","filterCanonical":"L","channel":"mono","exptimeSec":1.0,"meta":{},
            "gateVersion":0,"accepted":true,"state":"published","manifestVersion":1,
            "createdAt":"2026-09-27T00:00:00Z"}))
            .unwrap();
            v.publisher_device_id = Some("ME".into());
            derive_device_own(&mut v, &OwnDevices::only("ME"), None);
            assert!(v.own, "this device published it");
            v.publisher_device_id = Some("OTHER".into());
            derive_device_own(&mut v, &OwnDevices::only("ME"), None);
            assert!(!v.own, "another device of the SAME account: a replica here");

            v.publisher_device_id = None;
            derive_device_own(&mut v, &OwnDevices::only("ME"), None);
            assert!(!v.own, "unknown device, no own row here: a replica");
            let conn = Connection::open_in_memory().unwrap();
            crate::db::schema::init_db(&conn).unwrap();
            conn.execute(
                "INSERT INTO collab_projects
                (project_id, slug, title, data_role, target_name, target_ra_deg, target_dec_deg,
                 target_radius_deg, membership_version, snapshot_payload_b64,
                 snapshot_signature_b64, members_json)
             VALUES ('p1','m31','M31','send_receive','M31',10.7,41.3,1.5,1,'x','x','[]')",
                [],
            )
            .unwrap();
            let mut own = v.clone();
            own.own = true;
            crate::db::collab_frames::upsert_from_manifest(&conn, "p1", &own).unwrap();
            let without_path = crate::db::collab_frames::get(&conn, "p1", "u")
                .unwrap()
                .unwrap();
            derive_device_own(&mut v, &OwnDevices::only("ME"), Some(&without_path));
            assert!(!v.own, "an own row with no file here is not held as own");
            crate::db::collab_frames::update_landed_path(&conn, "p1", "u", "/x/u.fits").unwrap();
            let with_path = crate::db::collab_frames::get(&conn, "p1", "u")
                .unwrap()
                .unwrap();
            derive_device_own(&mut v, &OwnDevices::only("ME"), Some(&with_path));
            assert!(v.own, "unknown device, held here as own: stays own");
        }

        /// The pure classifier, one row at a time.
        #[test]
        fn classify_frame_change_rules() {
            use FramesChangeKind as K;
            let view = |own: bool, state: &str, accepted: bool, cv: i32| FrameViewWire {
                frame_uuid: "u".into(),
                frame_seq: 0,
                publisher_account_id: "a".into(),
                publisher_display_name: "A".into(),
                own,
                publisher_device_id: None,
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

    // ── Task 9 (wave 2): the replication pass against the fake hub ───────────

    /// What survives of the wave-2 replication tests (Task 15 retired the
    /// pass, the maintenance loop and the loss guard): a stale landing's
    /// file cleanup and the policy commands.
    #[cfg(all(feature = "render", feature = "solver"))]
    mod replication {
        use super::v3_fx::*;
        use super::*;
        use crate::sharing::iroh::node::SharedIrohNode;

        /// The v3 fixture with the context in an `Arc` (the loss command
        /// takes one), the project refreshed into the cache and the
        /// Collaboration root `<tmp>/Collab` designated. No node is bound.
        pub(super) struct RFx {
            pub _tmp: tempfile::TempDir,
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
                _tmp: tmp,
                ctx: Arc::new(ctx),
                hub,
                collab,
            }
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

        pub(super) fn pattern(seed: usize, len: usize) -> Vec<u8> {
            (0..len)
                .map(|j| ((j * 31 + seed * 97) % 251) as u8)
                .collect()
        }

        pub(super) async fn sync_rows(r: &RFx) {
            sync_manifest(&r.ctx, PID, None, None).await.unwrap();
        }

        /// Final review M3: a stale landing whose row cannot be re-read keeps
        /// its file (never the destructive default); once the row reads and
        /// does not reference the file, the file goes.
        #[tokio::test]
        async fn a_stale_landing_keeps_its_file_when_the_row_cannot_be_read() {
            let r = rfx("send_receive").await;
            let node = bind_receiver(&r).await;
            land_file(&r, "f1", FrameOrigin::Replica, &pattern(1, 256));
            let row = row(&r.ctx, "f1").unwrap();
            let stale = r.collab.join("m31").join("other").join("stale.fits");
            std::fs::write(&stale, b"stale bytes").unwrap();
            let project = {
                let conn = db(&r.ctx).unwrap().conn();
                crate::db::collab::get_project(&conn, PID).unwrap().unwrap()
            };
            let store = node.collab_store().unwrap();
            let guard = crate::collab::storage::marker::StoreGuard::new(
                r.collab.clone(),
                crate::api::account::own_device_id(&r.ctx).unwrap(),
                None,
            );
            let hooks = crate::sharing::iroh::blobs::ExportHooks::default();
            let started_at = crate::sync::now_iso();
            let env = crate::api::collab_live::landing::LandingEnv {
                ctx: &r.ctx,
                node: &node,
                store: &store,
                project: &project,
                collab_root: &r.collab,
                guard: &guard,
                started_at: &started_at,
                hooks: &hooks,
                sources: &[],
            };
            let tag = crate::sharing::iroh::node::project_frame_tag(PID, "f1", 1);
            let rename = |from: &str, to: &str| {
                db(&r.ctx)
                    .unwrap()
                    .conn()
                    .execute_batch(&format!("ALTER TABLE {from} RENAME TO {to}"))
                    .unwrap();
            };

            rename("project_frames_local", "pfl_aside");
            crate::api::collab_live::landing::forget_stale_landing(&env, &row, &tag, &stale).await;
            assert!(stale.exists(), "an unreadable row keeps the file");

            rename("pfl_aside", "project_frames_local");
            crate::api::collab_live::landing::forget_stale_landing(&env, &row, &tag, &stale).await;
            assert!(!stale.exists(), "an unreferenced stale file goes");
            node.shutdown().await;
        }

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
    }
}

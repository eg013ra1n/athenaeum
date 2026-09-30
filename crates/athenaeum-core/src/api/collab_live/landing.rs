//! Collab landing (collab v3 wave 3, Task 11; spec §7.5, I1, I6, I7, L5,
//! L7; plan P12, P13): a fetched frame's bytes go from the collab store to
//! their file under the Collaboration root, and the row becomes `held`.
//!
//! Moved here from the wave-2 `api::collab_exchange` and changed in three
//! ways:
//!
//! - **A new version replaces the old file atomically** — the old version
//!   stays whole until the new one is complete (I7, L7); there is no
//!   `remove_file(target)` first any more. Two exports (ruling R1,
//!   "variant C"), chosen per landing by [`landing_mode`]:
//!   - DIRECT: the blob is OWNED by the collab store and its data file sits
//!     on the target's device — iroh-blobs 0.103 exports it by one atomic
//!     rename onto the target and then serves from the target
//!     ([`blobs::export_child_direct`]);
//!   - TEMP: everything else (inline ≤ 16 KiB, already external,
//!     cross-device — cases the library writes the target IN PLACE) — export
//!     to `<target>.athtmp`, rename over the target, re-import it by
//!     reference ([`blobs::export_child_replacing`], P12).
//!
//!   Landings of one hash are serialized in-process ([`hash_landing_lock`])
//!   so nothing re-exports the entry between the Owned check and the export.
//! - **Nothing lands over a quarantined file** (P13, L5): the fence re-read
//!   ([`fresh_row`]) and the DB fence (`set_landed_if`) both require the row
//!   to be `wanted`; the wave-2 rename-aside of an edited file (R24) is gone
//!   — such a row can no longer get here. A `wanted` row whose stamp was
//!   verified at THIS version but whose file no longer holds the frame is
//!   refused too (an edit the storage engine has not ruled on yet).
//! - **The storage must be available for fetching** (§9.1): checked on the
//!   [`StoreGuard`] before any disk write.
//!
//! The row write is ONE transaction: `set_landed_if` (the landing fence, I1),
//! `set_local_state(held)` (its outbox `add`, C24) and the `sync_history` row.
//!
//! Ungated, unlike the rest of `api::collab_live` (it needs nothing render-
//! or solver-gated); the live executor (Task 15) is its caller.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};

use anyhow::{anyhow, Context, Result};

use crate::api::collab_exchange::{
    drop_tag, inside_root, project_disk_lock, publisher_folder, record_frame_error,
    size_mtime_from, xxh3_on_blocking,
};
use crate::api::{db, ApiError};
use crate::collab::storage::marker::StoreGuard;
use crate::collab::storage::states::{transition, StateEvent};
use crate::collab::storage::sweep::{stat_verdict, Stamp, StatVerdict};
use crate::db::collab::CollabProjectRow;
use crate::db::collab_frames::{self as frames_db, LocalFrameRow, LocalState};
use crate::services::ServiceContext;
use crate::sharing::iroh::blobs::{self, ExportHooks};
use crate::sharing::iroh::node::{project_frame_tag, SharedIrohNode};

/// Everything one landing needs: the catalog, the node and its mounted
/// collab store, the project (its id and slug name the landing folder), the
/// Collaboration root and the storage guard (§9.1).
pub struct LandingEnv<'a> {
    pub ctx: &'a ServiceContext,
    pub node: &'a SharedIrohNode,
    pub store: &'a iroh_blobs::api::Store,
    pub project: &'a CollabProjectRow,
    pub collab_root: &'a Path,
    pub guard: &'a StoreGuard,
    /// When the fetch that brought the bytes started (`sync_history`).
    pub started_at: &'a str,
    /// The exports' test seams — production passes a default value.
    pub(crate) hooks: &'a ExportHooks,
    /// Who delivered the frame's bytes, largest first (the exchange meter's
    /// per-device totals for this fetch; empty for a link). Recorded as the
    /// frame's sources by the receive-session writer (Task 13).
    pub sources: &'a [(String, u64)],
}

impl LandingEnv<'_> {
    fn pid(&self) -> &str {
        &self.project.project_id
    }
}

/// What landing (or linking) one frame did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Landed {
    Yes(PathBuf),
    /// The store's data went away under us (export found no source): the
    /// tags are dropped and the frame waits for GC (P20). Not a failure.
    AwaitingGc,
    /// The row moved on (a new version, other content, no longer `wanted`)
    /// while the bytes were in flight (R20, P13): nothing recorded, left for
    /// the next pass.
    Stale,
    /// The collaboration storage is not available for fetching (§9.1):
    /// nothing written, the in-flight bytes kept for a later landing.
    Unavailable,
    /// Logged and recorded on the row.
    Failed(String),
}

/// The in-flight tag of one frame version in the collab store (P22).
pub fn project_frame_in_flight_tag(
    project_id: &str,
    frame_uuid: &str,
    content_version: i32,
) -> String {
    blobs::in_flight_tag(&project_frame_tag(project_id, frame_uuid, content_version))
}

/// DIRECT or TEMP export (ruling R1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LandingMode {
    /// One atomic rename of the store-owned data file onto the target.
    Direct,
    /// `<target>.athtmp`, rename over the target, re-import by reference.
    Temp,
}

impl LandingMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            LandingMode::Direct => "direct",
            LandingMode::Temp => "temp",
        }
    }
}

/// Ruling R1: DIRECT only for a blob the collab store OWNS whose data file
/// is on the target's device — the one case iroh-blobs 0.103 exports by an
/// atomic rename. Inline and external entries, and an owned one on another
/// device (the library's EXDEV fallback copies onto the target in place; on
/// Windows the rename just fails), take TEMP. A DIRECT export that fails
/// anyway (not a vanished source) is retried once through TEMP — e.g.
/// Windows' ERROR_NOT_SAME_DEVICE for a volume mounted inside the root,
/// which the drive-letter device key cannot see (fix round 1, M2).
///
/// Known unsupported layout: a Linux bind mount inside the root reports the
/// SAME `st_dev` on both sides, yet `rename` across it fails with EXDEV, and
/// iroh-blobs then copies onto the target IN PLACE (the old version is not
/// kept whole while the copy runs). Nothing here can detect it cheaply.
pub(crate) fn landing_mode(owned: bool, same_device: bool) -> LandingMode {
    if owned && same_device {
        LandingMode::Direct
    } else {
        LandingMode::Temp
    }
}

/// Probe the inputs of [`landing_mode`] for `hash` landing at `dest`: the
/// mode, and the owned data file when DIRECT.
fn probe_mode(
    env: &LandingEnv<'_>,
    hash: &iroh_blobs::Hash,
    dest: &Path,
    size: u64,
) -> (LandingMode, Option<PathBuf>) {
    let Some(data) = env.node.collab_owned_data_path(hash) else {
        return (LandingMode::Temp, None);
    };
    // A complete entry at or below the inline limit lives in the store's
    // database, whatever files are around.
    let owned = size > blobs::INLINE_BLOB_MAX_BYTES && data.is_file();
    let same_device = owned
        && match dest.parent() {
            Some(parent) => {
                match (
                    crate::file_op::planner::device_id_for(&data),
                    crate::file_op::planner::device_id_for(parent),
                ) {
                    (Ok(a), Ok(b)) => a == b,
                    (a, b) => {
                        let e = a
                            .err()
                            .or(b.err())
                            .map(|e| format!("{e:#}"))
                            .unwrap_or_default();
                        tracing::warn!(path = %dest.display(), error = %e, "landing device check failed; exporting through a temp file");
                        false
                    }
                }
            }
            None => false,
        };
    let mode = landing_mode(owned, same_device);
    (mode, (mode == LandingMode::Direct).then_some(data))
}

/// The in-process lock serializing every landing of one hash (ruling R1):
/// held across the Owned check, the export and the record, so no other
/// landing turns the entry external in between. Entries are weak; dead ones
/// are pruned as the map grows.
fn hash_landing_lock(hash: &iroh_blobs::Hash) -> Arc<tokio::sync::Mutex<()>> {
    type Locks = (
        HashMap<iroh_blobs::Hash, Weak<tokio::sync::Mutex<()>>>,
        usize,
    );
    static LOCKS: OnceLock<std::sync::Mutex<Locks>> = OnceLock::new();
    let mut guard = LOCKS
        .get_or_init(|| std::sync::Mutex::new((HashMap::new(), 64)))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (map, prune_at) = &mut *guard;
    if let Some(lock) = map.get(hash).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    map.insert(*hash, Arc::downgrade(&lock));
    if map.len() > *prune_at {
        map.retain(|_, w| w.strong_count() > 0);
        *prune_at = (map.len() * 2).max(64);
    }
    lock
}

/// Drop one frame's seed tags (failures are logged inside
/// [`SharedIrohNode::unseed_project_frame`]).
async fn unseed(node: &SharedIrohNode, project_id: &str, frame_uuid: &str) {
    if node.collab_store().is_some() {
        let _ = node.unseed_project_frame(project_id, frame_uuid).await;
    }
}

/// The storage may be written for a fetch right now (§9.1); logged when not.
fn storage_fetching(env: &LandingEnv<'_>, row: &LocalFrameRow) -> bool {
    // The marker check without the write probe (Task 15 R4): the live
    // session's storage engine runs the full check every few seconds.
    let state = env.guard.check_marker();
    if state.fetching() {
        return true;
    }
    tracing::info!(
        project_id = env.pid(),
        frame_uuid = %row.frame_uuid,
        content_version = row.content_version,
        state = ?state,
        "collaboration storage not available; landing deferred"
    );
    false
}

/// The row as it is NOW, or `None` (logged) when it no longer describes the
/// bytes in hand — a manifest sync moved its version or content, it went
/// away (R20), it is already on disk (re-admitted meanwhile, N3), or it is
/// not `wanted` any more (P13: quarantined, not kept, awaiting a choice,
/// excluded).
pub(crate) fn fresh_row(
    env: &LandingEnv<'_>,
    row: &LocalFrameRow,
) -> Result<Option<LocalFrameRow>, ApiError> {
    let db = db(env.ctx)?;
    let fresh = frames_db::get(&db.conn(), env.pid(), &row.frame_uuid)?;
    match fresh {
        Some(f)
            if f.content_version == row.content_version
                && f.blake3 == row.blake3
                && !f.on_disk
                && f.local_state == LocalState::Wanted =>
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
fn new_landing_path(env: &LandingEnv<'_>, row: &LocalFrameRow) -> Result<PathBuf, String> {
    crate::package::validate_rel_path(&row.file_name)
        .map_err(|e| format!("unsafe file name {:?}: {e:#}", row.file_name))?;
    let dir = db(env.ctx)
        .map_err(anyhow::Error::from)
        .and_then(|db| {
            publisher_folder(
                &db.conn(),
                env.collab_root,
                env.project,
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
/// first — a NEW VERSION (a version bump clears `size_mtime_seen`) replaces
/// the old file under the same name (plan step 7.5, owner-approved; the old
/// file stays whole until the new one is complete, L7). Anything else goes
/// to [`new_landing_path`].
///
/// Returns the target and whether the file already there holds this frame's
/// content (size + xxh3) — then the caller must NOT export over it (final
/// review C1: when that file is the store entry's only data, an export over
/// it destroys the frame) and just records it.
///
/// A stamp recorded at THIS version (`size_mtime_seen` set on a `wanted`
/// row: re-included, or released from parking) means the file was verified
/// as this version; a file there that no longer holds it was edited, and an
/// edited file is never landed over (L5, spec §9.4 "Held ──content
/// changed──▶ Quarantined"): refused, for the storage engine to quarantine.
async fn landing_target(
    env: &LandingEnv<'_>,
    row: &LocalFrameRow,
) -> Result<(PathBuf, bool), TargetRefusal> {
    let existing = row
        .landed_path
        .as_deref()
        .map(PathBuf::from)
        .filter(|p| inside_root(Some(env.collab_root), row, p));
    let Some(dest) = existing else {
        return new_landing_path(env, row)
            .map(|p| (p, false))
            .map_err(TargetRefusal::Failed);
    };
    let holds = dest.exists() && holds_frame_content(&dest, row).await;
    if dest.exists() && !holds {
        if row.size_mtime_seen.is_some() {
            return Err(TargetRefusal::Edited(format!(
                "the file at {} changed since it was verified at this version; not landed over (L5)",
                dest.display()
            )));
        }
        // L5/C38 (fix round 1): a new version never replaces an old file
        // that no longer stats as it was last verified — an edit nothing
        // watched (the app closed, the watcher dead) is the user's.
        let prev = db(env.ctx)
            .map_err(anyhow::Error::from)
            .and_then(|db| frames_db::prev_stamp(&db.conn(), env.pid(), &row.frame_uuid))
            .map_err(|e| TargetRefusal::Failed(format!("read the previous stamp: {e:#}")))?;
        if let Some(prev) = prev.as_deref().and_then(Stamp::parse) {
            match stat_verdict(&dest, Some(prev)) {
                StatVerdict::Same | StatVerdict::Missing => {}
                StatVerdict::Drifted(now) => {
                    return Err(TargetRefusal::Edited(format!(
                        "the file at {} changed since its last verified version ({} → {}); \
                         the new version is not landed over it (L5)",
                        dest.display(),
                        prev.encode(),
                        now.encode()
                    )))
                }
                StatVerdict::Unreadable(e) => {
                    return Err(TargetRefusal::Failed(format!(
                        "stat {}: {e}",
                        dest.display()
                    )))
                }
            }
        }
    }
    unseed(env.node, env.pid(), &row.frame_uuid).await;
    Ok((dest, holds))
}

/// Why [`landing_target`] refused.
enum TargetRefusal {
    /// A local failure (logged and recorded by the caller).
    Failed(String),
    /// The file at the target is an edit the storage engine has not ruled
    /// on yet (L5): never landed over; the file goes to the engine, which
    /// quarantines it (spec §9.4), and the fetched bytes keep their
    /// in-flight tag so the frame is not fetched again meanwhile (M3).
    Edited(String),
}

/// Hand an edited target to the running storage engine (it quarantines the
/// frame, spec §9.4). Without a running engine nothing re-checks the file:
/// every landing pass refuses again (the fetched bytes keep their in-flight
/// tag) until an engine runs and quarantines it.
fn hand_to_engine(env: &LandingEnv<'_>, row: &LocalFrameRow, dest: &Path) {
    if crate::collab::storage::watch::route_touched(dest) {
        tracing::info!(project_id = env.pid(), frame_uuid = %row.frame_uuid, path = %dest.display(), "edited landing target handed to the storage engine");
    } else {
        tracing::debug!(project_id = env.pid(), frame_uuid = %row.frame_uuid, path = %dest.display(), "no storage engine runs; nothing re-checks the edited landing target and every pass refuses again until one runs (the fetched bytes keep their tag)");
    }
}

/// Does the file at `path` already hold this frame's content (size first,
/// then xxh3)? Then a re-land simply records it (N3).
async fn holds_frame_content(path: &Path, row: &LocalFrameRow) -> bool {
    match tokio::fs::metadata(path).await {
        Ok(m) if m.is_file() && m.len() as i64 == row.byte_size => {}
        _ => return false,
    }
    matches!(xxh3_on_blocking(path).await, Ok(h) if h == row.xxh3)
}

/// Land one fetched frame (P21) — under the hash's landing lock, then the
/// project's disk lock: re-read the row (R20, P13), check the storage
/// (§9.1), pick the target, export DIRECT or TEMP (module docs) — the landed
/// file IS the seed — then the permanent seed tag, then the in-flight tag
/// goes, then ONE DB transaction (fence + `held` + `sync_history`) that only
/// lands on the same version and content of a `wanted` row. A stale landing
/// drops its tags and removes nothing the row references.
///
/// A new version that REPLACED the frame's file in place leaves the old
/// hash's store entry reading that path; every other frame still holding
/// the old hash is parked (Task 15 R4 ruling (d)): unseeded and
/// `awaiting_gc`, re-adopted by hash from its own file once the collab GC
/// dropped the entry — no second copy on disk.
pub async fn land_frame(
    env: &LandingEnv<'_>,
    row: &LocalFrameRow,
    hash: iroh_blobs::Hash,
) -> Landed {
    let mut replaced = Vec::new();
    let landed = land_frame_locked(env, row, hash, &mut replaced).await;
    // After the landing's locks are released: parking takes each sibling's
    // project disk lock (this frame's own among them).
    // The live exchange (and its parking) exists only in a full build.
    #[cfg(all(feature = "render", feature = "solver"))]
    if let Landed::Yes(dest) = &landed {
        for old in replaced {
            park_replaced_siblings(env, row, &old, dest).await;
        }
    }
    #[cfg(not(all(feature = "render", feature = "solver")))]
    let _ = replaced;
    landed
}

/// Park every servable frame (another project's too) that still holds
/// `old` — the hash whose file at `dest` a new version just replaced.
#[cfg(all(feature = "render", feature = "solver"))]
async fn park_replaced_siblings(
    env: &LandingEnv<'_>,
    row: &LocalFrameRow,
    old: &iroh_blobs::Hash,
    dest: &Path,
) {
    let siblings = match db(env.ctx).and_then(|d| {
        Ok(frames_db::rows_with_blake3(
            &d.conn(),
            &old.to_hex().to_string(),
        )?)
    }) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(project_id = env.pid(), frame_uuid = %row.frame_uuid, blake3 = %old, error = %e, "frames sharing a replaced file's old content could not be read; they are left to the stat sweep");
            return;
        }
    };
    for sib in siblings {
        if (sib.project_id == row.project_id && sib.frame_uuid == row.frame_uuid)
            || !sib.local_state.servable()
        {
            continue;
        }
        let Some(path) = sib.landed_path.clone().map(PathBuf::from) else {
            continue;
        };
        if path == dest {
            continue;
        }
        tracing::warn!(
            project_id = %sib.project_id,
            frame_uuid = %sib.frame_uuid,
            path = %path.display(),
            blake3 = %old,
            "a new version replaced a file this frame's content was read from; parked until the entry is collected"
        );
        if let Err(e) =
            crate::api::collab_live::replace::park_row(env.ctx, env.node, &sib, &path).await
        {
            tracing::error!(project_id = %sib.project_id, frame_uuid = %sib.frame_uuid, error = %e, "frame sharing a replaced file could not be parked");
            continue;
        }
        // The storage engine re-adopts it by hash from its own file: at once
        // when the GC was quicker, else at its parked retry (one GC interval
        // later) — which the touch schedules.
        if !crate::collab::storage::watch::route_touched(&path) {
            tracing::debug!(project_id = %sib.project_id, frame_uuid = %sib.frame_uuid, path = %path.display(), "no storage engine watches the parked frame; its next sweep retries it");
        }
    }
}

async fn land_frame_locked(
    env: &LandingEnv<'_>,
    row: &LocalFrameRow,
    hash: iroh_blobs::Hash,
    replaced: &mut Vec<iroh_blobs::Hash>,
) -> Landed {
    let pid = env.pid();
    let uuid = row.frame_uuid.as_str();
    let in_flight = project_frame_in_flight_tag(pid, uuid, row.content_version);
    let fail = |msg: String| -> Landed {
        tracing::error!(project_id = pid, frame_uuid = uuid, error = %msg, "frame landing failed");
        record_frame_error(env.ctx, pid, uuid, &msg);
        Landed::Failed(msg)
    };
    let hash_lock = hash_landing_lock(&hash);
    let _hash_guard = hash_lock.lock().await;
    let disk_lock = match project_disk_lock(env.ctx, pid) {
        Ok(l) => l,
        Err(e) => return fail(format!("project disk lock: {e}")),
    };
    let _guard = disk_lock.lock().await;
    let row = match fresh_row(env, row) {
        Ok(Some(r)) => r,
        Ok(None) => {
            drop_tag(env.store, &in_flight).await;
            return Landed::Stale;
        }
        Err(e) => return fail(format!("re-read the frame: {e}")),
    };
    if !storage_fetching(env, &row) {
        return Landed::Unavailable;
    }
    // The hashes this frame seeded before (the target unseeds them): when
    // the new version replaces the file in place, their entries read a file
    // that no longer holds them (R4 ruling (d)).
    let old_seeds: Vec<iroh_blobs::Hash> = if row.landed_path.is_some() {
        match env.node.project_frame_tags(pid, uuid).await {
            Ok(tags) => tags
                .into_iter()
                .map(|(_, h)| h)
                .filter(|h| *h != hash)
                .collect(),
            Err(e) => {
                tracing::warn!(project_id = pid, frame_uuid = uuid, error = %format!("{e:#}"), "the frame's previous seeds could not be listed; frames sharing them are left to the stat sweep");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    let (dest, holds) = match landing_target(env, &row).await {
        Ok(d) => d,
        Err(TargetRefusal::Failed(msg)) => {
            drop_tag(env.store, &in_flight).await;
            return fail(msg);
        }
        Err(TargetRefusal::Edited(msg)) => {
            // M3: the in-flight tag stays — the bytes wait for the user's
            // choice instead of being fetched again every pass. A held-back
            // landing, not a failure: a warning, repeated each pass until
            // the engine quarantines the file (fix round 2).
            tracing::warn!(project_id = pid, frame_uuid = uuid, error = %msg, "frame landing held back over an edited file");
            if let Some(p) = row.landed_path.as_deref() {
                hand_to_engine(env, &row, Path::new(p));
            }
            record_frame_error(env.ctx, pid, uuid, &msg);
            return Landed::Failed(msg);
        }
    };
    // The target replaces the row's previous file: never removed on a later
    // failure (it is the frame's only copy by then; the next pass finds it
    // holding the frame and just records it, C1).
    let replaces = row.landed_path.as_deref().map(Path::new) == Some(dest.as_path());
    if let Some(parent) = dest.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            drop_tag(env.store, &in_flight).await;
            return fail(format!("create {}: {e}", parent.display()));
        }
    }
    let size = row.byte_size.max(0) as u64;
    // C1: the file at the target already IS this content (a version bump
    // with identical bytes, or a same-version re-land over an intact copy).
    // Exporting over it would — when that file is the store entry's only
    // data — destroy the frame. Tag and record it as is.
    let (mode, exported) = if holds {
        tracing::info!(project_id = pid, frame_uuid = uuid, path = %dest.display(), "landed file already holds the frame; no export");
        ("none", Ok(Ok(())))
    } else {
        match probe_mode(env, &hash, &dest, size) {
            (LandingMode::Direct, Some(data)) => {
                match blobs::export_child_direct(env.store, hash, &dest, size, &data, env.hooks)
                    .await
                {
                    // M2: a rename the device check could not foresee
                    // (Windows ERROR_NOT_SAME_DEVICE for a volume mounted
                    // inside the root): once more through the temp file.
                    Ok(Err(e)) if !blobs::export_source_vanished(&e) => {
                        tracing::warn!(project_id = pid, frame_uuid = uuid, path = %dest.display(), error = %e, "direct landing export failed; retrying through a temp file");
                        (
                            LandingMode::Temp.as_str(),
                            blobs::export_child_replacing(env.store, hash, &dest, size, env.hooks)
                                .await,
                        )
                    }
                    other => (LandingMode::Direct.as_str(), other),
                }
            }
            _ => (
                LandingMode::Temp.as_str(),
                blobs::export_child_replacing(env.store, hash, &dest, size, env.hooks).await,
            ),
        }
    };
    match exported {
        Err(e) => {
            drop_tag(env.store, &in_flight).await;
            return fail(format!("export to {}: {e:#}", dest.display()));
        }
        Ok(Err(e)) if blobs::export_source_vanished(&e) => {
            tracing::warn!(project_id = pid, frame_uuid = uuid, path = %dest.display(), error = %e, "frame data vanished before landing; waiting for GC");
            drop_tag(env.store, &in_flight).await;
            unseed(env.node, pid, uuid).await;
            match db(env.ctx) {
                Ok(db) => {
                    if let Err(e) = frames_db::set_awaiting_gc(&db.conn(), pid, uuid, true) {
                        tracing::warn!(project_id = pid, frame_uuid = uuid, error = %format!("{e:#}"), "mark frame awaiting GC failed");
                    }
                }
                Err(e) => {
                    tracing::warn!(project_id = pid, frame_uuid = uuid, error = %e, "mark frame awaiting GC failed")
                }
            }
            return Landed::AwaitingGc;
        }
        Ok(Err(e)) => {
            drop_tag(env.store, &in_flight).await;
            return fail(format!("export to {}: {e}", dest.display()));
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
        if !holds && !replaces {
            remove_landed(&dest);
        }
        drop_tag(env.store, &in_flight).await;
        return fail(format!("seed tag {tag}: {e}"));
    }
    drop_tag(env.store, &in_flight).await;
    match record_landing(env, &row, &dest) {
        Ok(true) => {
            tracing::info!(
                project_id = pid,
                frame_uuid = uuid,
                content_version = row.content_version,
                path = %dest.display(),
                mode,
                "frame landed"
            );
            if replaces && !holds {
                *replaced = old_seeds;
                replaced.sort_unstable();
                replaced.dedup();
            }
            Landed::Yes(dest)
        }
        Ok(false) => {
            forget_stale_landing(env, &row, &tag, &dest).await;
            Landed::Stale
        }
        Err(e) => {
            if !holds && !replaces {
                remove_landed(&dest);
            }
            unseed(env.node, pid, uuid).await;
            fail(format!("record landing: {e:#}"))
        }
    }
}

/// A landing the row moved away from while it was written (R20): drop the
/// seed tag it set, and remove the file only when the row does not reference
/// that path.
pub(crate) async fn forget_stale_landing(
    env: &LandingEnv<'_>,
    row: &LocalFrameRow,
    tag: &str,
    dest: &Path,
) {
    tracing::info!(
        project_id = env.pid(),
        frame_uuid = %row.frame_uuid,
        path = %dest.display(),
        "frame changed while landing; left for the next pass"
    );
    drop_tag(env.store, tag).await;
    // M3: a failed read must never pick the destructive default — keep the
    // file (at worst an inert leftover) and log.
    let current = match db(env.ctx) {
        Ok(db) => {
            frames_db::get(&db.conn(), env.pid(), &row.frame_uuid).map_err(|e| format!("{e:#}"))
        }
        Err(e) => Err(e.to_string()),
    };
    let referenced = match current {
        Ok(current) => current
            .and_then(|r| r.landed_path)
            .is_some_and(|p| Path::new(&p) == dest),
        Err(e) => {
            tracing::warn!(project_id = env.pid(), frame_uuid = %row.frame_uuid, path = %dest.display(), error = %e, "stale landing: re-reading the frame failed; the file is kept");
            true
        }
    };
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

/// ONE transaction: the landing fence (`set_landed_if` — same version and
/// content, still `wanted`; R20, I1, P13), the move to `held` through
/// `set_local_state` (on_disk + the outbox `add`, C24) and a `sync_history`
/// row for the receive, the way the package ingest wrote it. `Ok(false)` =
/// stale, nothing written.
fn record_landing(env: &LandingEnv<'_>, row: &LocalFrameRow, dest: &Path) -> Result<bool> {
    let meta = std::fs::metadata(dest).with_context(|| format!("stat {}", dest.display()))?;
    let sm = size_mtime_from(&meta);
    let to = transition(row.origin, LocalState::Wanted, StateEvent::Landed)
        .ok_or_else(|| anyhow!("a {:?} frame does not land", row.origin))?;
    let db = db(env.ctx).map_err(|e| anyhow!("{e}"))?;
    let mut conn = db.conn();
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .context("begin landing tx")?;
    let n = frames_db::set_landed_if(
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
    frames_db::set_local_state(&tx, &row.project_id, &row.frame_uuid, to)
        .context("move frame to held")?;
    let now = crate::sync::now_iso();
    crate::db::collab_sessions::record_landing(
        &tx,
        &row.project_id,
        &now,
        row.byte_size.max(0),
        env.sources,
    )
    .context("record receive session")?;
    crate::sync::store::insert_history_row(
        &tx,
        &crate::sync::HistoryRow {
            frame_uuid: row.frame_uuid.clone(),
            filename: row.file_name.clone(),
            object: None,
            // The device that delivered the most bytes; `local` for a copy
            // linked from content already on disk (`link_identical`, no
            // fetch — `env.sources` is empty).
            peer_device: env
                .sources
                .first()
                .map(|(d, _)| d.clone())
                .unwrap_or_else(|| "local".to_string()),
            direction: crate::sync::Direction::Received,
            bytes: row.byte_size.max(0) as u64,
            started_at: env.started_at.to_string(),
            finished_at: Some(now.clone()),
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
/// fetch. Under the hash's landing lock and the project's disk lock, on the
/// row as it is now (R20, P13). A new version replaces the old file through
/// `<target>.athtmp` + rename, never by deleting it first (I7, L7).
pub async fn link_identical(env: &LandingEnv<'_>, row: &LocalFrameRow, src: &Path) -> Landed {
    let pid = env.pid();
    let uuid = row.frame_uuid.as_str();
    let fail = |msg: String| -> Landed {
        tracing::warn!(project_id = pid, frame_uuid = uuid, error = %msg, "identical frame landing failed");
        record_frame_error(env.ctx, pid, uuid, &msg);
        Landed::Failed(msg)
    };
    let hash = match row.blake3.parse::<iroh_blobs::Hash>() {
        Ok(h) => h,
        Err(e) => return fail(format!("frame blake3 does not parse: {e}")),
    };
    let hash_lock = hash_landing_lock(&hash);
    let _hash_guard = hash_lock.lock().await;
    let disk_lock = match project_disk_lock(env.ctx, pid) {
        Ok(l) => l,
        Err(e) => return fail(format!("project disk lock: {e}")),
    };
    let _guard = disk_lock.lock().await;
    let row = match fresh_row(env, row) {
        Ok(Some(r)) => r,
        Ok(None) => return Landed::Stale,
        Err(e) => return fail(format!("re-read the frame: {e}")),
    };
    if !storage_fetching(env, &row) {
        return Landed::Unavailable;
    }
    tracing::warn!(project_id = pid, frame_uuid = uuid, path = %src.display(), "identical frame content in project");
    let dest = match landing_target(env, &row).await {
        Ok((d, _)) => d,
        Err(TargetRefusal::Failed(msg)) => return fail(msg),
        Err(TargetRefusal::Edited(msg)) => {
            hand_to_engine(
                env,
                &row,
                &PathBuf::from(row.landed_path.as_deref().unwrap_or_default()),
            );
            return fail(msg);
        }
    };
    let replaces = row.landed_path.as_deref().map(Path::new) == Some(dest.as_path());
    if dest != src {
        if let Some(parent) = dest.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return fail(format!("create {}: {e}", parent.display()));
            }
        }
        let tmp = blobs::athtmp_path(&dest);
        match std::fs::remove_file(&tmp) {
            Ok(()) => tracing::debug!(path = %tmp.display(), "stale landing temp file removed"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return fail(format!("remove stale temp {}: {e}", tmp.display())),
        }
        if let Err(e) = crate::sync::ingest::link_or_copy(src, &tmp, false) {
            return fail(format!(
                "link {} -> {}: {e:#}",
                src.display(),
                tmp.display()
            ));
        }
        // I1 (fix round 1): the source may have been edited since it
        // landed — its bytes are verified BEFORE they can replace the old
        // version.
        match blobs::blake3_on_blocking(&tmp).await {
            Ok(h) if h == hash => {}
            got => {
                let got = match got {
                    Ok(h) => h.to_string(),
                    Err(e) => format!("unreadable: {e:#}"),
                };
                remove_landed(&tmp);
                tracing::error!(project_id = pid, frame_uuid = uuid, path = %src.display(), expected = %hash, got = %got, "identical frame source does not hold the frame's bytes");
                return fail(format!(
                    "{} does not hash to {hash} (got {got}); the old version is kept",
                    src.display()
                ));
            }
        }
        if let Err(e) = std::fs::rename(&tmp, &dest) {
            remove_landed(&tmp);
            return fail(format!(
                "rename {} over {}: {e}",
                tmp.display(),
                dest.display()
            ));
        }
    }
    match env
        .node
        .seed_project_frame(pid, uuid, row.content_version, &dest)
        .await
    {
        Ok(seeded) if seeded == hash => {}
        Ok(seeded) => {
            // The file changed after its check (or `dest == src` was never
            // checked): never recorded held (I1).
            unseed(env.node, pid, uuid).await;
            if !replaces {
                remove_landed(&dest);
            }
            return fail(format!(
                "seed {}: hashed {seeded} instead of {hash}",
                dest.display()
            ));
        }
        Err(e) => {
            if !replaces {
                remove_landed(&dest);
            }
            return fail(format!("seed {}: {e:#}", dest.display()));
        }
    }
    match record_landing(env, &row, &dest) {
        Ok(true) => {
            tracing::info!(
                project_id = pid,
                frame_uuid = uuid,
                content_version = row.content_version,
                path = %dest.display(),
                mode = "link",
                "frame landed"
            );
            Landed::Yes(dest)
        }
        Ok(false) => {
            let tag = project_frame_tag(pid, uuid, row.content_version);
            forget_stale_landing(env, &row, &tag, &dest).await;
            Landed::Stale
        }
        Err(e) => {
            if !replaces {
                remove_landed(&dest);
            }
            unseed(env.node, pid, uuid).await;
            fail(format!("record landing: {e:#}"))
        }
    }
}

/// Remove every `<target>.athtmp` a crash or kill left under the
/// Collaboration root mid-landing (Task 15, the T11 M1 carry). Run by the
/// live session when it mounts the store, before any landing of the session
/// starts — so no live temp file is ever touched. `.athenaeum` (the store,
/// the marker) is never walked. Returns how many files went.
pub fn sweep_orphaned_athtmp(root: &Path) -> usize {
    let mut removed = 0usize;
    let walker = walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || e.file_name() != ".athenaeum");
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(path = %root.display(), error = %e, "landing temp sweep: a folder could not be read");
                continue;
            }
        };
        let is_temp = entry.file_type().is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|x| x == blobs::ATHTMP_EXT);
        if !is_temp {
            continue;
        }
        match std::fs::remove_file(entry.path()) {
            Ok(()) => removed += 1,
            Err(e) => {
                tracing::warn!(path = %entry.path().display(), error = %e, "landing temp sweep: an orphaned temp file could not be removed")
            }
        }
    }
    if removed > 0 {
        tracing::info!(path = %root.display(), count = removed, "orphaned landing temp files removed");
    }
    removed
}

/// A landed, on-disk frame of the same project with this content (P24).
pub fn identical_landed(
    ctx: &ServiceContext,
    row: &LocalFrameRow,
) -> Result<Option<PathBuf>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    Ok(
        frames_db::find_by_project_and_xxh3(&conn, &row.project_id, &row.xxh3)?
            .into_iter()
            .filter(|r| r.frame_uuid != row.frame_uuid && r.on_disk && r.blake3 == row.blake3)
            .filter_map(|r| r.landed_path.map(PathBuf::from))
            .find(|p| p.is_file()),
    )
}

// The rigs live in the render+solver-gated `test_support` (P1 headless rule).
#[cfg(all(test, feature = "render", feature = "solver"))]
mod tests {
    use super::*;
    use crate::api::collab_live::test_support as ts;
    use crate::db::collab_frames::{self as frames_db, LocalState};

    fn athtmp_files(root: &std::path::Path) -> usize {
        walkdir::WalkDir::new(root)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "athtmp"))
            .count()
    }

    #[cfg(unix)]
    fn ino(p: &Path) -> u64 {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(p).unwrap().ino()
    }

    #[test]
    fn only_an_owned_blob_on_the_targets_device_lands_direct() {
        assert_eq!(landing_mode(true, true), LandingMode::Direct);
        assert_eq!(
            landing_mode(true, false),
            LandingMode::Temp,
            "cross-device: the library's copy fallback writes in place"
        );
        assert_eq!(
            landing_mode(false, true),
            LandingMode::Temp,
            "inline or external"
        );
        assert_eq!(landing_mode(false, false), LandingMode::Temp);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_new_version_replaces_the_old_file_atomically_and_the_store_serves_the_target() {
        let rig = ts::fetch_rig(1).await; // a provider with v1 and a receiver that fetched v1's blob into its collab store
        let (pid, uuid) = rig.frame(0);
        let v1_path = rig.land(0).await.expect("v1 lands");
        rig.publish_new_version(0).await; // provider re-versions; receiver's row → wanted(v2), v1 file untouched
        assert_eq!(rig.row(0).local_state, LocalState::Wanted);
        assert_eq!(
            std::fs::read(&v1_path).unwrap(),
            rig.v1_bytes(0),
            "v1 stays until v2 replaces it (L7)"
        );
        rig.fetch_blob(0).await; // v2 bytes into the receiver's store
        let data = rig.node.collab_owned_data_path(&rig.v2_hash(0)).unwrap();
        assert!(data.is_file(), "the fetched v2 blob is owned by the store");
        #[cfg(unix)]
        let data_ino = ino(&data);
        let v2_path = rig.land(0).await.expect("v2 lands");
        assert_eq!(v2_path, v1_path, "same path");
        assert_eq!(std::fs::read(&v2_path).unwrap(), rig.v2_bytes(0));
        assert_eq!(athtmp_files(&rig.root), 0);
        assert_eq!(
            (rig.hooks.direct_exports(), rig.hooks.temp_exports()),
            (2, 0),
            "both versions landed DIRECT"
        );
        #[cfg(unix)]
        assert_eq!(
            ino(&v2_path),
            data_ino,
            "v2 is the store's data file, renamed over v1"
        );
        assert!(!data.exists());
        // P12: the store references the TARGET, not a dead temp name
        crate::sharing::iroh::blobs::probe_first_byte(&rig.receiver_store(), rig.v2_hash(0))
            .await
            .expect("readable through the target");
        let conn = crate::api::db(&rig.ctx).unwrap().conn();
        let row = frames_db::get(&conn, &pid, &uuid).unwrap().unwrap();
        assert_eq!(
            (row.local_state, row.content_version),
            (LocalState::Held, 2)
        );
        assert!(row.on_disk);
        let ops: Vec<_> = crate::db::collab_live::outbox(&conn, &pid)
            .unwrap()
            .into_iter()
            .map(|o| o.op)
            .collect();
        assert_eq!(
            ops.last(),
            Some(&crate::db::collab_live::ClaimOp::Add { content_version: 2 }),
            "the landing appends its outbox add: {ops:?}"
        );
        drop(conn);
        assert_eq!(
            rig.node.project_frame_tags(&pid, &uuid).await.unwrap(),
            vec![(2, rig.v2_hash(0))],
            "v1 unseeded, v2 seeded"
        );
    }

    /// Task 13: a landed frame's sources (the fetch's per-device byte
    /// totals) join the project's receive session and name the
    /// `sync_history` row's real top source — no more the placeholder
    /// `"swarm"`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_landed_frames_sources_join_its_receive_session_and_name_its_history_row() {
        let rig = ts::fetch_rig(1).await;
        let (pid, uuid) = rig.frame(0);
        rig.land_with_sources(0, &[("SRC=".into(), 42)])
            .await
            .expect("v1 lands");

        let conn = crate::api::db(&rig.ctx).unwrap().conn();
        let sessions = crate::db::collab_sessions::list(&conn, Some(&pid), 10).unwrap();
        assert_eq!(sessions.len(), 1, "one receive session for the landing");
        assert_eq!(sessions[0].frames, 1);
        assert_eq!(sessions[0].sources.get("SRC=").copied(), Some(42));

        let rows = crate::sync::store::search_history_rows(
            &conn,
            &crate::sync::HistoryQuery {
                project: Some(pid.clone()),
                limit: 10,
                ..Default::default()
            },
        )
        .unwrap();
        let row = rows
            .iter()
            .find(|r| r.frame_uuid == uuid)
            .expect("a sync_history row for the landed frame");
        assert_eq!(
            row.peer_device, "SRC=",
            "the history row names the real top source, not the swarm placeholder"
        );
    }

    /// DIRECT path (ruling R1): the only local step before the export is
    /// the export itself — a fault injected before it leaves the old file
    /// whole; the retry lands.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_direct_landing_that_fails_before_its_export_keeps_the_old_file() {
        let rig = ts::fetch_rig(1).await;
        let v1_path = rig.land(0).await.unwrap();
        rig.publish_new_version(0).await;
        rig.fetch_blob(0).await;
        rig.hooks.fail_before_export_once();
        assert!(matches!(rig.land(0).await, Err(Landed::Failed(_))));
        assert_eq!(
            std::fs::read(&v1_path).unwrap(),
            rig.v1_bytes(0),
            "the old file is intact (I7)"
        );
        assert_eq!(rig.row(0).local_state, LocalState::Wanted);
        // M1: a stale temp file of an earlier TEMP attempt is cleared by the
        // DIRECT landing.
        std::fs::write(
            crate::sharing::iroh::blobs::athtmp_path(&v1_path),
            b"a stale temp",
        )
        .unwrap();
        let again = rig.land(0).await.unwrap();
        assert_eq!(again, v1_path);
        assert_eq!(std::fs::read(&again).unwrap(), rig.v2_bytes(0));
        assert_eq!(
            (rig.hooks.direct_exports(), rig.hooks.temp_exports()),
            (2, 0)
        );
        assert_eq!(athtmp_files(&rig.root), 0);
    }

    /// M2: a DIRECT export the store refuses (as a rename across volumes
    /// the device check could not see) is retried once through TEMP.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_direct_export_is_retried_through_a_temp_file() {
        let rig = ts::fetch_rig(1).await;
        let v1_path = rig.land(0).await.unwrap();
        rig.publish_new_version(0).await;
        rig.fetch_blob(0).await;
        rig.hooks.fail_direct_export_once();
        let v2_path = rig.land(0).await.expect("lands through the temp file");
        assert_eq!(v2_path, v1_path);
        assert_eq!(std::fs::read(&v2_path).unwrap(), rig.v2_bytes(0));
        assert_eq!(
            (rig.hooks.direct_exports(), rig.hooks.temp_exports()),
            (2, 1)
        );
        assert_eq!(athtmp_files(&rig.root), 0);
        crate::sharing::iroh::blobs::probe_first_byte(&rig.receiver_store(), rig.v2_hash(0))
            .await
            .unwrap();
        assert_eq!(rig.row(0).local_state, LocalState::Held);
    }

    /// I1: an EXTERNAL entry whose referenced file was edited (same size)
    /// is copied from that file — the TEMP landing verifies the temp's
    /// BLAKE3 before the rename, refuses, and leaves the old version whole.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_temp_landing_from_an_edited_external_file_is_refused_before_the_rename() {
        let rig = ts::fetch_rig(2).await;
        let a = rig.land(0).await.unwrap();
        let b = rig.land(1).await.unwrap();
        rig.publish_version_with(1, rig.v1_bytes(0)).await;
        rig.fetch_blob(1).await;
        ts::overwrite_same_size(&a); // the entry's only path now holds other bytes
        assert!(
            matches!(rig.land(1).await, Err(Landed::Failed(m)) if m.contains("does not hash")),
            "refused by the BLAKE3 check"
        );
        assert_eq!(
            std::fs::read(&b).unwrap(),
            rig.v1_bytes(1),
            "v1 intact (I7)"
        );
        assert_eq!(athtmp_files(&rig.root), 0, "the unverified temp is removed");
        let row = rig.row(1);
        assert!(row.local_state == LocalState::Wanted && !row.on_disk);
    }

    /// I1: `link_identical` from a source edited since it landed is refused
    /// before the rename — the old version whole, the row never held.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn linking_from_an_edited_source_is_refused() {
        let rig = ts::fetch_rig(2).await;
        let a = rig.land(0).await.unwrap();
        let b = rig.land(1).await.unwrap();
        rig.publish_version_with(1, rig.v1_bytes(0)).await;
        ts::overwrite_same_size(&a);
        assert!(
            matches!(rig.link(1, &a).await, Err(Landed::Failed(m)) if m.contains("does not hash")),
            "refused by the BLAKE3 check"
        );
        assert_eq!(
            std::fs::read(&b).unwrap(),
            rig.v1_bytes(1),
            "v1 intact (I7)"
        );
        assert_eq!(athtmp_files(&rig.root), 0);
        let row = rig.row(1);
        assert!(row.local_state == LocalState::Wanted && !row.on_disk);
        assert!(rig
            .node
            .project_frame_tags(&row.project_id, &row.frame_uuid)
            .await
            .unwrap()
            .is_empty());
    }

    struct TwoHolders;
    impl crate::api::collab_live::storage_task::HolderView for TwoHolders {
        fn other_holders(&self, _: &str, _: &str) -> crate::collab::live::holders::Redundancy {
            crate::collab::live::holders::Redundancy {
                online: 2,
                total: 2,
            }
        }
    }

    /// I2 — owner rule L5/C38: the user's edit is never overwritten, not
    /// even by a new version. v1 held → edited while the app was closed (no
    /// engine saw it) → the publisher releases v2: the landing refuses (the
    /// file no longer stats as v1 was verified), keeps the fetched bytes
    /// (M3), and hands the file to the engine, which quarantines it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_new_version_never_lands_over_an_edit_nothing_saw() {
        let rig = ts::fetch_rig(1).await;
        let (pid, uuid) = rig.frame(0);
        let path = rig.land(0).await.unwrap();
        ts::overwrite_same_size(&path); // the app is closed: no engine pass
        let edited = std::fs::read(&path).unwrap();
        rig.publish_new_version(0).await;
        assert_eq!(rig.row(0).local_state, LocalState::Wanted);
        rig.fetch_blob(0).await;
        let mut eng = rig.engine();
        assert!(
            matches!(rig.land(0).await, Err(Landed::Failed(m)) if m.contains("(L5)")),
            "refused by the previous-stamp check"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            edited,
            "the edit is intact (L5)"
        );
        let in_flight = project_frame_in_flight_tag(&pid, &uuid, 2);
        assert!(
            rig.receiver_store()
                .tags()
                .get(&in_flight)
                .await
                .unwrap()
                .is_some(),
            "the fetched bytes keep their in-flight tag (M3)"
        );
        let t0 = std::time::Instant::now();
        eng.tick(t0, &TwoHolders).await;
        eng.tick(t0 + crate::collab::storage::watch::AGGREGATE, &TwoHolders)
            .await;
        assert_eq!(
            rig.row(0).local_state,
            LocalState::Quarantined,
            "handed to the engine"
        );
        assert_eq!(std::fs::read(&path).unwrap(), edited);
        let conn = crate::api::db(&rig.ctx).unwrap().conn();
        assert_eq!(
            frames_db::prev_stamp(&conn, &pid, &uuid).unwrap(),
            None,
            "cleared on entering quarantined"
        );
        assert_eq!(
            crate::db::collab_live::list_quarantine(&conn, &pid)
                .unwrap()
                .len(),
            1
        );
    }

    /// I-1 (Task 11 fix round 2) — owner rule L5/C38 across an exclusion:
    /// v1 held → excluded (`idle`, the engine ignores it) → edited in place
    /// → the publisher releases v2 while the frame is still idle → re-
    /// included. The bump keeps v1's verified stamp as `prev_stamp` on the
    /// idle row and re-inclusion keeps it, so the engine's pass quarantines
    /// the edit and nothing lands over it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_edit_made_while_excluded_is_never_landed_over_by_a_new_version() {
        use crate::api::collab_live::storage_task::apply_policy;
        let rig = ts::fetch_rig(1).await;
        let (pid, uuid) = rig.frame(0);
        let path = rig.land(0).await.unwrap();
        assert_eq!(rig.row(0).local_state, LocalState::Held);
        let mut eng = rig.engine();
        crate::db::collab::set_policy(
            &crate::api::db(&rig.ctx).unwrap().conn(),
            &pid,
            r#"{"filters":["Ha"]}"#,
        )
        .unwrap();
        apply_policy(&rig.ctx, &pid).unwrap();
        assert_eq!(rig.row(0).local_state, LocalState::Idle);
        ts::overwrite_same_size(&path); // idle: the engine ignores it
        let edited = std::fs::read(&path).unwrap();
        rig.publish_new_version(0).await;
        assert_eq!(rig.row(0).local_state, LocalState::Idle, "v2 while idle");
        crate::db::collab::set_policy(&crate::api::db(&rig.ctx).unwrap().conn(), &pid, "{}")
            .unwrap();
        apply_policy(&rig.ctx, &pid).unwrap();
        assert_eq!(rig.row(0).local_state, LocalState::Wanted, "re-included");
        let t0 = std::time::Instant::now();
        eng.tick(t0, &TwoHolders).await;
        eng.tick(t0 + crate::collab::storage::watch::AGGREGATE, &TwoHolders)
            .await;
        assert_eq!(
            rig.row(0).local_state,
            LocalState::Quarantined,
            "the engine's pass quarantines the edit made while excluded"
        );
        assert_eq!(std::fs::read(&path).unwrap(), edited, "the edit is intact");
        // v2 arrives anyway: nothing lands over the quarantined file.
        rig.fetch_blob(0).await;
        assert!(rig.land(0).await.is_err(), "nothing lands (P13)");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            edited,
            "the edit is byte-identical (L5)"
        );
        let conn = crate::api::db(&rig.ctx).unwrap().conn();
        assert_eq!(
            crate::db::collab_live::list_quarantine(&conn, &pid)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            frames_db::prev_stamp(&conn, &pid, &uuid).unwrap(),
            None,
            "cleared on entering quarantined"
        );
    }

    /// The same, untouched: v2 replaces v1 and the previous stamp goes.
    /// Task 15 R4 ruling (d): a new version replaced frame 0's file in
    /// place; frame 1 holds the OLD content at its own path, and the old
    /// hash's store entry reads the replaced file. Frame 1 is parked
    /// (unseeded, `awaiting_gc`) and — once the GC dropped the entry —
    /// re-adopted by hash from its own file: servable again, no second copy.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_replaced_file_parks_an_identical_sibling_until_the_gc() {
        use std::sync::atomic::Ordering;
        let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        crate::sharing::iroh::node::test_gc::arm(Some(std::sync::Arc::clone(&gate)));
        let rig = ts::fetch_rig(2).await;
        crate::sharing::iroh::node::test_gc::arm(None);
        let (pid, sib) = rig.frame(1);
        let path0 = rig.land(0).await.unwrap();
        let old = rig.v1_bytes(0);
        rig.publish_version_with(1, old.clone()).await;
        let path1 = rig.link(1, &path0).await.unwrap();
        assert_ne!(path0, path1);
        let old_hash = iroh_blobs::Hash::new(&old);
        // frame 0's new version replaces its file in place
        rig.publish_new_version(0).await;
        rig.fetch_blob(0).await;
        assert_eq!(rig.land(0).await.unwrap(), path0);
        let parked = rig.row(1);
        assert_eq!(
            (parked.local_state, parked.awaiting_gc),
            (LocalState::Wanted, true),
            "the sibling is parked"
        );
        assert_eq!(
            parked.landed_path.as_deref(),
            Some(path1.to_string_lossy().as_ref())
        );
        assert!(
            rig.node
                .project_frame_tags(&pid, &sib)
                .await
                .unwrap()
                .is_empty(),
            "unseeded: the entry can be collected"
        );
        gate.store(true, Ordering::SeqCst);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        while rig.node.collab_blob_health(old_hash).await.unwrap()
            != crate::sharing::iroh::node::BlobHealth::Missing
        {
            assert!(
                tokio::time::Instant::now() < deadline,
                "GC never dropped the entry"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        gate.store(false, Ordering::SeqCst);
        struct Nobody;
        impl crate::api::collab_live::storage_task::HolderView for Nobody {
            fn other_holders(&self, _: &str, _: &str) -> crate::collab::live::holders::Redundancy {
                Default::default()
            }
        }
        rig.engine().sweep(&Nobody).await;
        let back = rig.row(1);
        assert_eq!(
            back.local_state,
            LocalState::Held,
            "re-adopted after the GC"
        );
        assert_eq!(
            back.landed_path.as_deref(),
            Some(path1.to_string_lossy().as_ref())
        );
        assert_eq!(
            rig.node.collab_blob_health(old_hash).await.unwrap(),
            crate::sharing::iroh::node::BlobHealth::Readable,
            "served from the sibling's own file"
        );
        assert_eq!(std::fs::read(&path1).unwrap(), old);
        let frames = walkdir::WalkDir::new(&rig.root)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
            .filter(|e| !e.path().components().any(|c| c.as_os_str() == ".athenaeum"))
            .count();
        assert_eq!(frames, 2, "no second copy on disk");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_untouched_old_version_is_replaced_and_its_previous_stamp_cleared() {
        let rig = ts::fetch_rig(1).await;
        let (pid, uuid) = rig.frame(0);
        rig.land(0).await.unwrap();
        rig.publish_new_version(0).await;
        {
            let conn = crate::api::db(&rig.ctx).unwrap().conn();
            assert!(
                frames_db::prev_stamp(&conn, &pid, &uuid).unwrap().is_some(),
                "kept by the bump"
            );
        }
        rig.fetch_blob(0).await;
        let path = rig.land(0).await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), rig.v2_bytes(0));
        let conn = crate::api::db(&rig.ctx).unwrap().conn();
        assert_eq!(frames_db::prev_stamp(&conn, &pid, &uuid).unwrap(), None);
    }

    /// TEMP path: an inline blob (≤ 16 KiB — the library writes it with
    /// `File::create`) is exported to `<target>.athtmp`. A landing
    /// interrupted between that export and the rename keeps the old file
    /// whole; the retry reuses the completed temp file — counted: no second
    /// export.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_inline_frame_lands_through_a_temp_file_and_an_interrupted_landing_keeps_the_old_file(
    ) {
        let rig = ts::fetch_rig_sized(1, 8 * 1024).await;
        let v1_path = rig.land(0).await.unwrap();
        assert_eq!(
            (rig.hooks.direct_exports(), rig.hooks.temp_exports()),
            (0, 1),
            "inline: TEMP"
        );
        rig.publish_new_version(0).await;
        rig.fetch_blob(0).await;
        rig.hooks.fail_after_export_once();
        assert!(matches!(rig.land(0).await, Err(Landed::Failed(_))));
        assert_eq!(
            std::fs::read(&v1_path).unwrap(),
            rig.v1_bytes(0),
            "the old file is intact (I7)"
        );
        assert_eq!(
            athtmp_files(&rig.root),
            1,
            "the completed temp file waits beside it"
        );
        assert_eq!(rig.hooks.temp_exports(), 2);
        // the retry reuses the completed temp file: no second export
        let again = rig.land(0).await.unwrap();
        assert_eq!(again, v1_path);
        assert_eq!(std::fs::read(&again).unwrap(), rig.v2_bytes(0));
        assert_eq!(rig.hooks.temp_exports(), 2, "no second export of v2");
        assert_eq!(athtmp_files(&rig.root), 0);
        crate::sharing::iroh::blobs::probe_first_byte(&rig.receiver_store(), rig.v2_hash(0))
            .await
            .unwrap();
        assert_eq!(rig.row(0).local_state, LocalState::Held);
    }

    /// TEMP path: a blob already EXTERNAL — the same bytes landed at another
    /// frame's path, so the store references that file (the library would
    /// reflink-or-copy onto the target, in place). Frame 1's v2 is frame 0's
    /// bytes: v1 stays whole until the rename, frame 0's file is untouched,
    /// and the store still serves.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_external_blob_lands_through_a_temp_file_and_keeps_the_old_file_until_the_rename() {
        let rig = ts::fetch_rig(2).await;
        let a = rig.land(0).await.unwrap();
        let b = rig.land(1).await.unwrap();
        assert_eq!(rig.hooks.direct_exports(), 2);
        rig.publish_version_with(1, rig.v1_bytes(0)).await;
        rig.fetch_blob(1).await;
        let hash = iroh_blobs::Hash::new(rig.v1_bytes(0));
        assert!(
            !rig.node.collab_owned_data_path(&hash).unwrap().exists(),
            "external: no store-owned data file"
        );
        rig.hooks.fail_after_export_once();
        assert!(matches!(rig.land(1).await, Err(Landed::Failed(_))));
        assert_eq!(
            std::fs::read(&b).unwrap(),
            rig.v1_bytes(1),
            "frame 1's v1 is intact (I7)"
        );
        let again = rig.land(1).await.unwrap();
        assert_eq!(again, b);
        assert_eq!(std::fs::read(&b).unwrap(), rig.v1_bytes(0));
        assert_eq!(
            std::fs::read(&a).unwrap(),
            rig.v1_bytes(0),
            "frame 0's file untouched"
        );
        assert_eq!(
            (rig.hooks.direct_exports(), rig.hooks.temp_exports()),
            (2, 1)
        );
        assert_eq!(athtmp_files(&rig.root), 0);
        crate::sharing::iroh::blobs::probe_first_byte(&rig.receiver_store(), hash)
            .await
            .unwrap();
        assert_eq!(rig.row(1).local_state, LocalState::Held);
    }

    /// Task 15 (T11 M1 carry): the mount-time sweep removes orphaned
    /// `.athtmp` files anywhere under the root, never under `.athenaeum`,
    /// and never a frame file.
    #[test]
    fn the_mount_sweep_removes_orphaned_temp_files_only() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let dir = root.join("m31").join("pub");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(root.join(".athenaeum").join("blobs")).unwrap();
        std::fs::write(dir.join("a.fits"), b"frame").unwrap();
        std::fs::write(dir.join("a.fits.athtmp"), b"half").unwrap();
        std::fs::write(root.join("b.fits.athtmp"), b"half").unwrap();
        std::fs::write(
            root.join(".athenaeum").join("blobs").join("x.athtmp"),
            b"store",
        )
        .unwrap();
        assert_eq!(sweep_orphaned_athtmp(root), 2);
        assert!(dir.join("a.fits").exists(), "a frame file is never touched");
        assert!(
            root.join(".athenaeum")
                .join("blobs")
                .join("x.athtmp")
                .exists(),
            ".athenaeum is never walked"
        );
        assert_eq!(athtmp_files(root), 1);
        assert_eq!(sweep_orphaned_athtmp(root), 0, "idempotent");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn nothing_lands_over_a_quarantined_file() {
        let rig = ts::fetch_rig(1).await;
        let (pid, uuid) = rig.frame(0);
        let path = rig.land(0).await.unwrap();
        ts::overwrite_same_size(&path);
        let edited = std::fs::read(&path).unwrap();
        {
            let conn = crate::api::db(&rig.ctx).unwrap().conn();
            frames_db::set_local_state(&conn, &pid, &uuid, LocalState::Quarantined).unwrap();
        }
        rig.publish_new_version(0).await;
        rig.fetch_blob(0).await;
        assert_eq!(rig.land(0).await, Err(Landed::Stale));
        assert_eq!(
            std::fs::read(&path).unwrap(),
            edited,
            "the user's edit is never overwritten (L5)"
        );
        assert_eq!(rig.row(0).local_state, LocalState::Quarantined);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_version_bump_during_the_landing_is_fenced() {
        let rig = ts::fetch_rig(1).await;
        rig.fetch_blob(0).await;
        rig.bump_manifest_version_locally(0); // the manifest moved while the bytes were in flight
        assert_eq!(rig.land(0).await, Err(Landed::Stale));
        assert_eq!(athtmp_files(&rig.root), 0);
        let row = rig.row(0);
        assert!(row.landed_path.is_none() && !row.on_disk);
    }

    /// §9.1: an unavailable store (its marker gone) writes nothing; the
    /// bytes stay under their in-flight tag for a later landing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_landing_waits_while_the_storage_is_unavailable() {
        let rig = ts::fetch_rig(1).await;
        let (pid, uuid) = rig.frame(0);
        let marker = rig.root.join(crate::collab::storage::marker::MARKER_REL);
        let saved = std::fs::read(&marker).unwrap();
        std::fs::remove_file(&marker).unwrap();
        assert_eq!(rig.land(0).await, Err(Landed::Unavailable));
        assert!(rig.row(0).landed_path.is_none());
        let in_flight = project_frame_in_flight_tag(&pid, &uuid, 1);
        assert!(
            rig.receiver_store()
                .tags()
                .get(&in_flight)
                .await
                .unwrap()
                .is_some(),
            "the in-flight bytes are kept"
        );
        std::fs::write(&marker, saved).unwrap();
        let path = rig.land(0).await.expect("lands once the storage is back");
        assert_eq!(std::fs::read(&path).unwrap(), rig.v1_bytes(0));
    }

    /// A `wanted` row whose stamp was verified at this version (re-included,
    /// released from parking) and whose file was edited meanwhile is never
    /// landed over (L5) — left for the storage engine to quarantine.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_edited_file_verified_at_this_version_is_not_landed_over() {
        let rig = ts::fetch_rig(1).await;
        let (pid, uuid) = rig.frame(0);
        let path = rig.land(0).await.unwrap();
        ts::overwrite_same_size(&path);
        let edited = std::fs::read(&path).unwrap();
        {
            // back to `wanted` keeping the stamp — as a re-inclusion does
            let conn = crate::api::db(&rig.ctx).unwrap().conn();
            frames_db::set_local_state(&conn, &pid, &uuid, LocalState::Wanted).unwrap();
        }
        rig.fetch_blob(0).await;
        assert!(matches!(rig.land(0).await, Err(Landed::Failed(_))));
        assert_eq!(std::fs::read(&path).unwrap(), edited);
        assert_eq!(rig.row(0).local_state, LocalState::Wanted);
        assert!(
            rig.receiver_store()
                .tags()
                .get(&project_frame_in_flight_tag(&pid, &uuid, 1))
                .await
                .unwrap()
                .is_some(),
            "the fetched bytes keep their in-flight tag (M3)"
        );
    }
}

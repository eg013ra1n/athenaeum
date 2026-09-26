//! The storage marker's device-facing half (spec §9.5, plan P22/P29): is the
//! marker's device one of this account's OWN devices (offer a replace,
//! never a foreign store), the verified device-replace core (retire the old
//! device, rewrite the marker to name this one), and the user-confirmed,
//! no-hub-call take-over for a marker naming a device this account cannot
//! vouch for — both re-adopt everything already sitting in the folder by
//! content hash (the files never move).
//!
//! Fix round 2, ruling 1: NO automatic takeover anywhere. Absence from this
//! account's active device list is not proof of same-account ownership (it
//! could be another account's device, a plain-revoked device, or a swapped
//! disk). `replace_device` only ever acts on a VERIFIED active device;
//! [`take_over_collab_folder`] is the explicit, user-confirmed alternative
//! for everything else.

use std::path::{Path, PathBuf};

use crate::api::{db, ApiError, PathPolicy};
use crate::collab::storage::marker::{
    offer_flags, read_marker, writable, write_marker, StoreMarker,
};
use crate::db::collab_frames::{FrameOrigin, LocalFrameRow, LocalState};
use crate::services::ServiceContext;
use crate::sharing::iroh::node::SharedIrohNode;

/// What replacing the marker's device would look like, resolved against
/// this account's own device list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaceOffer {
    /// The hub's device id (what `revoke`/`replace_device` take — NOT the
    /// pubkey the marker itself names).
    pub device_id: String,
    pub device_pubkey: String,
    pub device_name: String,
    pub last_seen_at: Option<String>,
    pub offline_days: Option<i64>,
    pub prompt: bool,
    pub propose_retire: bool,
}

/// Whether `marker_device` (a storage marker's `device_id`, i.e. its
/// pubkey) is one of THIS account's own devices. `None` = it belongs to no
/// device of this account at all (a foreign store — a NAS folder shared
/// with someone else's install, or a marker this account never wrote) and
/// is never offered for replace.
pub async fn replace_offer(
    ctx: &ServiceContext,
    marker_device: &str,
) -> Result<Option<ReplaceOffer>, ApiError> {
    let devices = crate::api::account::list_devices(ctx).await?;
    let Some(dev) = devices.into_iter().find(|d| d.pubkey == marker_device) else {
        return Ok(None);
    };
    let last_seen = dev
        .last_seen_at
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&chrono::Utc));
    let (offline_days, prompt, propose_retire) = offer_flags(last_seen, chrono::Utc::now());
    Ok(Some(ReplaceOffer {
        device_id: dev.id,
        device_pubkey: dev.pubkey,
        device_name: dev.name,
        last_seen_at: dev.last_seen_at,
        offline_days,
        prompt,
        propose_retire,
    }))
}

/// The result of a device replace: how many files under the Collaboration
/// root were walked, and how many were re-adopted (matched a cached frame by
/// content hash and re-linked to it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaceOutcome {
    pub scanned: usize,
    pub adopted: usize,
}

/// Every local precondition a folder must clear before anything external or
/// irreversible happens to it (fix round 2, ruling 4): `root` exists, a dry
/// run of `set_collaboration_dir`'s own designation rules (path policy,
/// overlap, R5 promotion — [`crate::api::scan_roots::validate_folder_candidate`],
/// never mutating) passes, the folder is writable, and an iroh node is
/// bound. Shared by [`replace_device`] (before the hub retire call) and
/// [`take_over_collab_folder`] (which has no hub call to protect but must
/// still fail before rewriting anything).
async fn check_designation_preconditions(
    ctx: &ServiceContext,
    root: &Path,
    policy: &PathPolicy,
) -> Result<(), ApiError> {
    if !root.is_dir() {
        tracing::warn!(path = %root.display(), "collaboration folder precondition failed: not a folder");
        return Err(ApiError::Invalid(format!(
            "{} is not an existing folder",
            root.display()
        )));
    }
    // `validate_folder_candidate` answers "would a NEW designation of this
    // path succeed" — it does not special-case "this IS already the
    // designated Collaboration root" the way `designate_collaboration_root`'s
    // real `Kept` branch does (its `role_taken` check fires on ANY existing
    // collaboration root, before ever comparing paths). The idempotent
    // re-run case (the marker already names this device, root already
    // designated) must skip the dry run entirely rather than trip over its
    // own prior designation.
    let root_canon = root
        .canonicalize()
        .map(|p| crate::api::scan_roots::normalize_path(&p))
        .unwrap_or_else(|_| root.to_path_buf());

    // Fix round 3, point 1: a record for THIS canonical path whose store id
    // disagrees with the on-disk marker means a different disk now sits
    // here (or the marker was swapped underneath us) — refuse now, before
    // any hub call or rewrite. `set_collaboration_dir`'s own marker check
    // would eventually catch this too (`check_store`'s `MarkerMismatch`
    // arm), but by then a `replace_device` retire call is already
    // irreversible.
    check_no_recorded_marker_mismatch(ctx, root, &root_canon)?;

    let already_the_collab_root = crate::api::scan_roots::get_collaboration_dir(ctx)?
        .map(std::path::PathBuf::from)
        .is_some_and(|current| current == root_canon);
    if !already_the_collab_root {
        let verdict = crate::api::scan_roots::validate_folder_candidate(
            ctx,
            "collaboration".to_string(),
            root.to_string_lossy().to_string(),
            policy,
        )?;
        if !verdict.ok {
            let reason = verdict.reason.unwrap_or_default();
            tracing::warn!(path = %root.display(), reason = %reason, "collaboration folder precondition failed: cannot be designated");
            return Err(ApiError::Invalid(format!(
                "this folder cannot be designated as the Collaboration root: {reason}"
            )));
        }
    }
    if !writable(root) {
        tracing::warn!(path = %root.display(), "collaboration folder precondition failed: not writable");
        return Err(ApiError::Invalid(format!(
            "{} is not writable",
            root.display()
        )));
    }
    if crate::api::collab_exchange::bound_node(ctx).await.is_none() {
        tracing::warn!(path = %root.display(), "collaboration folder precondition failed: no iroh node bound");
        return Err(ApiError::Internal("no iroh node bound".to_string()));
    }
    Ok(())
}

/// Fix round 3, point 1: refuse when this catalog already has a recorded
/// storage marker for `root_canon` (i.e. `root` IS — or was — the
/// designated Collaboration root) whose `store_id` does not match the
/// marker actually sitting on disk right now. A mismatch here means the
/// physical disk changed underneath the path (or the marker was swapped by
/// something else) — the SAME `MarkerMismatch` [`crate::collab::storage::marker::check_store`]
/// would report, surfaced early so nothing irreversible happens first. A
/// missing record, or one for a different path, is not this function's
/// concern — it returns `Ok`.
fn check_no_recorded_marker_mismatch(
    ctx: &ServiceContext,
    root: &Path,
    root_canon: &Path,
) -> Result<(), ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    if crate::db::collab_live::store_marker_path(&conn)?.as_deref()
        != Some(root_canon.to_string_lossy().as_ref())
    {
        return Ok(());
    }
    let Some(recorded) = crate::db::collab_live::recorded_store_marker(&conn)? else {
        return Ok(());
    };
    let on_disk = match read_marker(root) {
        Ok(Some(m)) => m,
        Ok(None) => return Ok(()),
        Err(e) => {
            tracing::warn!(
                path = %root.display(),
                error = %e,
                "collaboration folder marker unreadable while checking for a recorded mismatch"
            );
            return Ok(());
        }
    };
    if on_disk.store_id != recorded.store_id {
        tracing::warn!(
            path = %root.display(),
            store_id = %on_disk.store_id,
            recorded_store_id = %recorded.store_id,
            "collaboration folder precondition failed: the on-disk marker's store id disagrees with the one this catalog recorded for this path"
        );
        return Err(ApiError::Conflict(
            "collaboration folder is not usable: MarkerMismatch".to_string(),
        ));
    }
    Ok(())
}

/// Walk `root` (skipping `.athenaeum` and `*.athtmp`) re-adopting every file
/// already there by content hash. Shared by [`replace_device`] and
/// [`take_over_collab_folder`]. Returns `(scanned, adopted)`.
async fn walk_and_adopt(
    ctx: &ServiceContext,
    node: &SharedIrohNode,
    root: &Path,
) -> (usize, usize) {
    let mut scanned = 0usize;
    let mut adopted = 0usize;
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| e.file_name() != ".athenaeum")
    {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(path = %root.display(), error = %e, "walk entry failed");
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "athtmp") {
            continue;
        }
        scanned += 1;
        match adopt_by_hash(ctx, node, root, path).await {
            Ok(landed) => adopted += landed.len(),
            Err(e) => tracing::warn!(path = %path.display(), error = %e, "adopt by hash failed"),
        }
    }
    (scanned, adopted)
}

/// The node bound on `ctx`, with its collab store confirmed mounted — the
/// common "ready to adopt" gate for [`replace_device`] and
/// [`take_over_collab_folder`], both called right after their own
/// `set_collaboration_dir`.
async fn require_mounted_node(
    ctx: &ServiceContext,
    root: &Path,
) -> Result<std::sync::Arc<SharedIrohNode>, ApiError> {
    let node = crate::api::collab_exchange::bound_node(ctx)
        .await
        .ok_or_else(|| {
            tracing::error!(path = %root.display(), "no iroh node bound; cannot adopt");
            ApiError::Internal("no collaboration store available to adopt into".to_string())
        })?;
    if node.collab_store().is_none() {
        tracing::error!(path = %root.display(), "the collaboration store is not mounted; cannot adopt");
        return Err(ApiError::Internal(
            "the collaboration store is not mounted".to_string(),
        ));
    }
    Ok(node)
}

/// Replace `device_id` (a hub device id, e.g. from a [`ReplaceOffer`]) as the
/// owner of `root` — a Collaboration root that is PENDING designation (the
/// reinstall flow: a plain `set_collaboration_dir` on this path would refuse
/// with `collab_other_device`, since the marker still names the device being
/// replaced) or already designated (a re-run after a partial failure).
///
/// Fix round 2, ruling 2: acts ONLY on a device that IS in this account's
/// active device list AND whose pubkey equals the marker's `device_id` — an
/// unlisted `device_id` refuses outright, with NO hub call (it cannot be
/// verified as belonging to this account at all; the recovery path for a
/// folder whose marker names a device that is no longer listed is
/// [`take_over_collab_folder`], not a `replace_device` retry). The one
/// exception is when the on-disk marker ALREADY names this device — a prior
/// run of THIS SAME replace got that far — which skips the active-list
/// check, the retire call and the marker rewrite entirely (idempotent
/// re-run) and only designates + adopts.
///
/// Fix round 2, ruling 4: every local precondition
/// ([`check_designation_preconditions`]) runs BEFORE the retire call, which
/// is the one step this function cannot cleanly undo. Only once all of that
/// holds does it retire the old device on the hub (spec §9.5 — a retired
/// device is never re-offered; a typed 404 — [`ApiError::NotFound`], not a
/// string match — for an already-retired device is treated as done, not a
/// failure, covering a race between the active-list check and the retire
/// call itself), rewrite the marker to name this device under the SAME
/// store id, designate `root` as the Collaboration root (its own storage
/// check now passes, since the marker already names this device) and mount
/// it directly, then walk the folder re-adopting every file already there
/// by content hash.
///
/// Fix round 2, ruling 5: uses the CANONICAL root `set_collaboration_dir`
/// returns for the walk and every `landed_path`/`.athenaeum` path from that
/// point on — `root` itself may not be in the canonical spelling the rest
/// of the catalog stores (a `/var` vs `/private/var` symlink, say). No
/// marker is recorded before designation (ruling 5) — `set_collaboration_dir`'s
/// own storage check records it once its mount actually succeeds (ruling 6).
pub async fn replace_device(
    ctx: &ServiceContext,
    device_id: &str,
    root: &Path,
    policy: &PathPolicy,
) -> Result<ReplaceOutcome, ApiError> {
    // Fix round 3, point 2: the policy gate runs BEFORE any filesystem probe
    // (`root.is_dir()`, `read_marker`) or hub call (`list_devices`) — on the
    // raw, uncanonicalized `root`, since canonicalizing is itself a probe.
    // This is a coarse, information-hiding pre-filter (the web backend must
    // not let a caller learn whether a marker exists at a path outside its
    // allowed roots); the authoritative, canonicalized policy check still
    // happens inside `set_collaboration_dir`'s own `validate_transfer_dir`.
    policy.check(root)?;

    let me = crate::api::account::own_device_id(ctx)?;

    let marker = match read_marker(root) {
        Ok(Some(m)) => m,
        Ok(None) => {
            return Err(ApiError::Invalid(
                "this folder has no collaboration storage marker to replace".to_string(),
            ))
        }
        Err(e) => {
            tracing::error!(path = %root.display(), error = %e, "device replace: reading the storage marker failed");
            return Err(ApiError::Internal(format!("read storage marker: {e}")));
        }
    };
    let already_replaced = marker.device_id == me;

    if !already_replaced {
        let Some(dev) = crate::api::account::list_devices(ctx)
            .await?
            .into_iter()
            .find(|d| d.id == device_id)
        else {
            tracing::warn!(
                path = %root.display(),
                device_id,
                "device replace refused: not an active device of this account"
            );
            return Err(ApiError::Invalid(
                "this device is not an active device of your account".to_string(),
            ));
        };
        if dev.pubkey != marker.device_id {
            tracing::warn!(
                path = %root.display(),
                requested = device_id,
                device_id = %marker.device_id,
                "device replace refused: the marker names a different device"
            );
            return Err(ApiError::Invalid(
                "the storage marker names a different device than the one being replaced"
                    .to_string(),
            ));
        }

        check_designation_preconditions(ctx, root, policy).await?;

        match crate::api::account::revoke_device_retire(ctx, device_id.to_string()).await {
            Ok(()) => {}
            Err(ApiError::NotFound(_)) => {
                tracing::info!(
                    device_id,
                    "device replace: the device was already retired; continuing"
                );
            }
            Err(e) => return Err(e),
        }

        let new_marker = StoreMarker {
            store_id: marker.store_id.clone(),
            device_id: me.clone(),
        };
        write_marker(root, &new_marker).map_err(|e| {
            tracing::error!(path = %root.display(), error = %e, "device replace: writing the storage marker failed");
            ApiError::Internal(format!("write storage marker: {e}"))
        })?;
    }

    // Designate (or confirm) `root` as the Collaboration root and mount it
    // DIRECTLY (`set_collaboration_dir` mounts through `mount_collab_store`,
    // unconditional — never the rate-limited `ensure_collab_store` lazy
    // path, which could still be sitting on the pre-replace OtherDevice
    // latch for up to 60s). The marker now names this device, so the
    // designation's own storage check passes and records it once the mount
    // commits.
    let stored = crate::api::scan_roots::set_collaboration_dir(
        ctx,
        root.to_string_lossy().to_string(),
        policy,
    )
    .await?;
    let canon_root = PathBuf::from(&stored);

    let node = require_mounted_node(ctx, &canon_root).await?;
    let (scanned, adopted) = walk_and_adopt(ctx, &node, &canon_root).await;

    tracing::info!(
        device_id,
        scanned,
        count = adopted,
        "collaboration folder re-adopted after a device replace"
    );
    Ok(ReplaceOutcome { scanned, adopted })
}

/// Take over a Collaboration folder whose marker names a device this
/// account cannot vouch for (an "UnknownDevice" refusal — another account's
/// device, a plain-revoked device, a swapped disk, or simply a device the
/// hub could not be asked about) — the user-confirmed alternative to
/// [`replace_device`] for exactly the case it now refuses outright (fix
/// round 2, ruling 2 and 3). NO hub call at all: `confirmed` must be `true`
/// (the caller's explicit confirmation — Task 16/17 own the actual prompt
/// and the UnknownDevice-only gating on when to offer this action), the
/// folder must carry a RECORDED `UnknownDevice` refusal naming exactly this
/// marker (fix round 3, point 3 — a fresh, hub-free "not in the active
/// list" re-classification is NOT sufficient: that list can be stale, or
/// simply not asked, and offering a take-over on that basis alone could
/// steal a folder out from under a device this account just hasn't been
/// able to reach), every local precondition runs first exactly like a
/// fresh designation ([`check_designation_preconditions`]), then the marker
/// is rewritten to name this device (keeping the existing marker's store id
/// when readable — this is still the same physical store, just under new,
/// unverified stewardship), the folder is designated and mounted, and its
/// contents are adopted by hash.
pub async fn take_over_collab_folder(
    ctx: &ServiceContext,
    root: &Path,
    policy: &PathPolicy,
    confirmed: bool,
) -> Result<ReplaceOutcome, ApiError> {
    // Fix round 3, point 2: the policy gate runs BEFORE any filesystem probe
    // or hub call — see the identical comment in `replace_device`.
    policy.check(root)?;

    if !confirmed {
        return Err(ApiError::Invalid(
            "user confirmation is required to take over this folder".to_string(),
        ));
    }

    let marker = match read_marker(root) {
        Ok(Some(m)) => m,
        Ok(None) => {
            return Err(ApiError::Invalid(
                "this folder has no collaboration storage marker to take over".to_string(),
            ))
        }
        Err(e) => {
            tracing::error!(path = %root.display(), error = %e, "take over: reading the storage marker failed");
            return Err(ApiError::Internal(format!("read storage marker: {e}")));
        }
    };

    // Fix round 3, point 3: require a RECORDED `Unknown` refusal for this
    // exact (path, device) — never re-derive "unknown" fresh from the
    // active-device list here.
    let root_canon = root
        .canonicalize()
        .map(|p| crate::api::scan_roots::normalize_path(&p))
        .unwrap_or_else(|_| root.to_path_buf());
    let root_canon_str = root_canon.to_string_lossy().to_string();
    {
        let db = db(ctx)?;
        let conn = db.conn();
        let refusal = crate::db::collab_live::refused_designation(&conn)?;
        let matches = refusal.as_ref().is_some_and(|(path, device_id, kind)| {
            *path == root_canon_str
                && *device_id == marker.device_id
                && *kind == crate::db::collab_live::RefusedDeviceKind::Unknown
        });
        if !matches {
            tracing::warn!(
                path = %root.display(),
                device_id = %marker.device_id,
                "take over refused: no recorded unknown-device refusal for this folder's marker"
            );
            return Err(ApiError::Invalid(
                "this folder was not refused as an unrecognized device's storage — designate it \
                 normally, or use replace if the device is still active"
                    .to_string(),
            ));
        }
    }

    check_designation_preconditions(ctx, root, policy).await?;

    let me = crate::api::account::own_device_id(ctx)?;
    let new_marker = StoreMarker {
        store_id: marker.store_id.clone(),
        device_id: me,
    };
    write_marker(root, &new_marker).map_err(|e| {
        tracing::error!(path = %root.display(), error = %e, "take over: writing the storage marker failed");
        ApiError::Internal(format!("write storage marker: {e}"))
    })?;

    let stored = crate::api::scan_roots::set_collaboration_dir(
        ctx,
        root.to_string_lossy().to_string(),
        policy,
    )
    .await?;
    let canon_root = PathBuf::from(&stored);

    let node = require_mounted_node(ctx, &canon_root).await?;
    let (scanned, adopted) = walk_and_adopt(ctx, &node, &canon_root).await;

    tracing::info!(
        scanned,
        count = adopted,
        path = %canon_root.display(),
        "collaboration folder taken over and re-adopted"
    );
    Ok(ReplaceOutcome { scanned, adopted })
}

/// Seed `landed_path`'s bytes into the store for `row` (reference import —
/// the file never moves), verify the BLAKE3 the store reads back against
/// the manifest, and — once verified — record `landed_path` under one
/// transaction. A mismatch unseeds the frame again (fix round 1: never
/// leaves a wrongly-tagged blob pinned) and reports no adoption.
/// What [`land_candidate`] did with one candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Landing {
    /// Seeded, verified and recorded: the state move it made.
    Landed { from: LocalState, to: LocalState },
    /// A moved frame whose re-seed hit a dead store entry — parked
    /// ([`park_row`]) until the collab GC drops the entry.
    Parked,
    /// Not adopted (seed failure, content mismatch); nothing recorded.
    Refused,
}

pub(crate) async fn land_candidate(
    ctx: &ServiceContext,
    node: &SharedIrohNode,
    row: &LocalFrameRow,
    landed_path: &Path,
) -> Result<Landing, ApiError> {
    let landed_str = landed_path.to_string_lossy().to_string();
    let hash = match node
        .seed_project_frame(
            &row.project_id,
            &row.frame_uuid,
            row.content_version,
            landed_path,
        )
        .await
    {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(
                project_id = %row.project_id,
                frame_uuid = %row.frame_uuid,
                path = %landed_str,
                error = %format!("{e:#}"),
                "adopt by hash: seed failed"
            );
            if is_moved_class(row) {
                park_row(ctx, node, row, landed_path).await?;
                return Ok(Landing::Parked);
            }
            return Ok(Landing::Refused);
        }
    };
    if hash.to_string() != row.blake3 {
        tracing::warn!(
            project_id = %row.project_id,
            frame_uuid = %row.frame_uuid,
            path = %landed_str,
            blake3 = %hash,
            expected = %row.blake3,
            "adopt by hash: content does not match the manifest; skipped"
        );
        if let Err(e) = node
            .unseed_project_frame(&row.project_id, &row.frame_uuid)
            .await
        {
            tracing::warn!(
                project_id = %row.project_id,
                frame_uuid = %row.frame_uuid,
                error = %e,
                "adopt by hash: unseed after a hash mismatch failed"
            );
        }
        return Ok(Landing::Refused);
    }

    let target_state = if row.origin == FrameOrigin::Own {
        LocalState::OwnHeld
    } else {
        LocalState::Held
    };
    // Task 11 (ledger ruling): written through the storage engine's fenced
    // transaction — the row must still be what the caller read (state,
    // landed path, content version, stamp); a row that moved on meanwhile
    // (a manifest bump, a landing, a user action) is never turned `held`
    // from a stale view. The seed tag this set stays (same name for a
    // concurrent landing of this version; a new version's landing unseeds
    // every tag of the frame first).
    let lock = crate::api::collab_exchange::project_disk_lock(ctx, &row.project_id)?;
    let _guard = lock.lock().await;
    let hash_str = hash.to_string();
    let write = crate::api::collab_live::storage_task::frame_tx_locked(ctx, row, |tx| {
        crate::db::collab_frames::update_landed_path(
            tx,
            &row.project_id,
            &row.frame_uuid,
            &landed_str,
        )?;
        if let Ok(meta) = std::fs::metadata(landed_path) {
            crate::db::collab_frames::set_size_mtime_seen(
                tx,
                &row.project_id,
                &row.frame_uuid,
                &crate::api::collab_exchange::size_mtime_from(&meta),
            )?;
        }
        // The file hashed to the row's confirmed blake3: an own row is no
        // longer staged (Task 10, C11).
        tx.execute(
            "UPDATE project_frames_local SET awaiting_gc = 0, rejected_size_mtime = NULL,
                own_staged = CASE WHEN blake3 = ?3 THEN 0 ELSE own_staged END
             WHERE project_id = ?1 AND frame_uuid = ?2",
            rusqlite::params![row.project_id, row.frame_uuid, hash_str],
        )?;
        Ok(crate::db::collab_frames::set_local_state(
            tx,
            &row.project_id,
            &row.frame_uuid,
            target_state,
        )?)
    })?;
    if write.is_none() {
        tracing::info!(
            project_id = %row.project_id,
            frame_uuid = %row.frame_uuid,
            path = %landed_str,
            "adopt by hash: the frame moved on meanwhile; not recorded"
        );
        return Ok(Landing::Refused);
    }
    if is_moved_class(row) {
        tracing::info!(
            project_id = %row.project_id,
            frame_uuid = %row.frame_uuid,
            src = row.landed_path.as_deref().unwrap_or_default(),
            path = %landed_str,
            "moved frame re-adopted by hash"
        );
    }
    Ok(match write {
        Some(w) => Landing::Landed {
            from: w.from,
            to: w.to,
        },
        None => Landing::Refused,
    })
}

/// Task 9: a `held`/`own_held` row whose recorded file no longer exists — a
/// move inside the root (spec §9.4 "Held ──moved inside the root──▶ Held,
/// re-adopted by hash, path updated, no transfer").
fn is_moved_class(row: &LocalFrameRow) -> bool {
    matches!(row.local_state, LocalState::Held | LocalState::OwnHeld)
        && row
            .landed_path
            .as_deref()
            .is_some_and(|p| !Path::new(p).exists())
}

/// Park `row` at `landed_path` (fix round 1: also the C10 sibling of a
/// dead entry, at its own path). A moved frame whose re-seed at its new
/// path failed. The usual cause is
/// iroh-blobs 0.103's external-path UNION (`blobs::ensure_child_readable`):
/// the entry keeps reading the OLD, now-dead path when it sorts first, and
/// the collab store never copy-repairs (P20). The bytes are verified (size +
/// xxh3 matched), so the frame is recorded at its new path, its seed tags
/// dropped so the collab GC can collect the dead entry (P31), and it is
/// parked `wanted` with `awaiting_gc` — not servable (a dead entry serves
/// nothing), never fetched while parked. The storage engine retries parked
/// rows one collab GC interval later and on every sweep: re-seeded from the
/// recorded path once the entry is gone, released to a plain fetch when the
/// file no longer matches.
pub(crate) async fn park_row(
    ctx: &ServiceContext,
    node: &SharedIrohNode,
    row: &LocalFrameRow,
    landed_path: &Path,
) -> Result<(), ApiError> {
    if let Err(e) = node
        .unseed_project_frame(&row.project_id, &row.frame_uuid)
        .await
    {
        tracing::warn!(
            project_id = %row.project_id,
            frame_uuid = %row.frame_uuid,
            error = %e,
            "moved frame: unseed before parking failed"
        );
    }
    let parked = if row.origin == FrameOrigin::Own {
        LocalState::OwnMissing
    } else {
        LocalState::Wanted
    };
    let landed_str = landed_path.to_string_lossy().to_string();
    // Task 11 (ledger ruling): the storage engine's fenced transaction — a
    // row that moved on since the caller read it is left to the newer write.
    let lock = crate::api::collab_exchange::project_disk_lock(ctx, &row.project_id)?;
    let _guard = lock.lock().await;
    let written = crate::api::collab_live::storage_task::frame_tx_locked(ctx, row, |tx| {
        crate::db::collab_frames::update_landed_path(
            tx,
            &row.project_id,
            &row.frame_uuid,
            &landed_str,
        )?;
        if let Ok(meta) = std::fs::metadata(landed_path) {
            crate::db::collab_frames::set_size_mtime_seen(
                tx,
                &row.project_id,
                &row.frame_uuid,
                &crate::api::collab_exchange::size_mtime_from(&meta),
            )?;
        }
        tx.execute(
            "UPDATE project_frames_local SET awaiting_gc = 1 WHERE project_id = ?1 AND frame_uuid = ?2",
            rusqlite::params![row.project_id, row.frame_uuid],
        )?;
        crate::db::collab_frames::set_local_state(tx, &row.project_id, &row.frame_uuid, parked)?;
        Ok(Some(()))
    })?;
    if written.is_none() {
        tracing::info!(
            project_id = %row.project_id,
            frame_uuid = %row.frame_uuid,
            path = %landed_str,
            "moved frame changed meanwhile; not parked"
        );
        return Ok(());
    }
    tracing::warn!(
        project_id = %row.project_id,
        frame_uuid = %row.frame_uuid,
        path = %landed_str,
        to_state = parked.as_db_str(),
        "moved frame cannot be re-seeded until the collab store drops its dead entry; parked"
    );
    Ok(())
}

/// Match a file already sitting in the Collaboration root, by content hash,
/// to every cached project frame it satisfies — the device-replace
/// re-adoption step (and, later tasks, the same disk-truth
/// "moved/duplicate" leg per frame). Scoped by `(size, xxh3)` across every
/// project this device caches ([`crate::db::collab_frames::project_ids`],
/// mirroring the scanner's own reconciliation).
///
/// C10 ("identical bytes in two frames or projects — blob ≠ reference"):
/// every servable-or-own candidate is adopted, not just the first. The
/// candidate whose OWN project folder contains `path` (matched by slug
/// against `path`'s first component under `root`) is preferred as the
/// PRIMARY — it keeps `path` as its `landed_path` — since a file usually
/// still sits where its own project would have put it; every other match
/// gets its own on-disk copy (hardlinked where the filesystem allows it,
/// `landed_path` is a UNIQUE column) under `<root>/.athenaeum/adopted/`.
///
/// An `idle` candidate (caps-excluded, not yet accepted/published) only
/// gets its path recorded, under [`crate::api::collab_exchange::project_disk_lock`]
/// — never served, its state stays `idle` — and only when there is no
/// servable-or-own match at all. Returns one `(project_id, frame_uuid)` per
/// adoption that landed (verified content, transaction committed).
pub(crate) async fn adopt_by_hash(
    ctx: &ServiceContext,
    node: &SharedIrohNode,
    root: &Path,
    path: &Path,
) -> Result<Vec<(String, String)>, ApiError> {
    Ok(adopt_by_hash_detailed(ctx, node, root, path)
        .await?
        .adopted
        .into_iter()
        .map(|a| (a.project_id, a.frame_uuid))
        .collect())
}

/// One adoption [`adopt_by_hash_detailed`] landed: the frame and the state
/// move it made (`from == to` for a moved `held` row — no outbox row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Adopted {
    pub project_id: String,
    pub frame_uuid: String,
    pub from: LocalState,
    pub to: LocalState,
}

/// [`adopt_by_hash_detailed`]'s answer: the adoptions that landed, and
/// whether the file matched ANY cached frame by `(size, xxh3)` at all (an
/// idle frame's recorded path, a duplicate of a frame already held — a file
/// that matched is never listed under "Other files").
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AdoptOutcome {
    pub adopted: Vec<Adopted>,
    pub matched: bool,
    /// Moved frames parked over a dead store entry (the engine schedules a
    /// retry for them).
    pub parked: Vec<(String, String)>,
}

/// [`adopt_by_hash`] with each adoption's state move and the "matched at
/// all" flag — the storage engine's re-adoption leg (Task 9). Candidate
/// classes: `wanted`, `missing`, `awaiting_choice`, `not_kept` (put back),
/// `own_missing` (back), and — Task 9 — a `held`/`own_held` row whose
/// recorded file no longer exists (moved inside the root: re-seeded at the
/// new path, path updated, stays held, no outbox row).
pub(crate) async fn adopt_by_hash_detailed(
    ctx: &ServiceContext,
    node: &SharedIrohNode,
    root: &Path,
    path: &Path,
) -> Result<AdoptOutcome, ApiError> {
    let size = match tokio::fs::metadata(path).await {
        Ok(m) => m.len() as i64,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "adopt by hash: stat failed");
            return Ok(AdoptOutcome::default());
        }
    };
    // Fix round 1: no full hash for a file whose size no cached frame has.
    let sized = {
        let db = db(ctx)?;
        crate::db::collab_frames::any_with_byte_size(&db.conn(), size)?
    };
    if !sized {
        return Ok(AdoptOutcome::default());
    }
    let xxh3 = match crate::api::collab_exchange::xxh3_on_blocking(path).await {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %format!("{e:#}"), "adopt by hash: hash failed");
            return Ok(AdoptOutcome::default());
        }
    };
    let path_str = path.to_string_lossy().to_string();
    let local_folder = path
        .strip_prefix(root)
        .ok()
        .and_then(|rel| rel.components().next())
        .map(|c| c.as_os_str().to_string_lossy().to_string());

    let db = db(ctx)?;
    let (servable_or_own, idle, matched) = {
        let conn = db.conn();
        let mut candidates: Vec<LocalFrameRow> = Vec::new();
        for project_id in crate::db::collab_frames::project_ids(&conn)? {
            candidates.extend(
                crate::db::collab_frames::find_by_project_and_xxh3(&conn, &project_id, &xxh3)?
                    .into_iter()
                    .filter(|r| r.byte_size == size),
            );
        }
        if let Some(folder) = &local_folder {
            let mut slugs: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            for c in &candidates {
                if !slugs.contains_key(&c.project_id) {
                    let slug = crate::db::collab::get_project(&conn, &c.project_id)?
                        .map(|p| p.slug)
                        .unwrap_or_default();
                    slugs.insert(c.project_id.clone(), slug);
                }
            }
            // A stable sort: candidates whose own project's slug matches
            // `path`'s top-level folder move first; every other relative
            // ordering is left as found.
            candidates.sort_by_key(|c| {
                slugs
                    .get(&c.project_id)
                    .map(|s| s != folder)
                    .unwrap_or(true)
            });
        }
        let matched = !candidates.is_empty();
        let servable_or_own: Vec<LocalFrameRow> = candidates
            .iter()
            .filter(|r| {
                matches!(
                    r.local_state,
                    LocalState::Wanted
                        | LocalState::Missing
                        | LocalState::AwaitingChoice
                        | LocalState::NotKept
                        | LocalState::OwnMissing
                ) || is_moved_class(r)
            })
            .cloned()
            .collect();
        let idle = if servable_or_own.is_empty() {
            candidates
                .into_iter()
                .find(|r| r.local_state == LocalState::Idle)
        } else {
            None
        };
        (servable_or_own, idle, matched)
    };

    if servable_or_own.is_empty() {
        if let Some(idle) = idle {
            let lock = crate::api::collab_exchange::project_disk_lock(ctx, &idle.project_id)?;
            let _guard = lock.lock().await;
            let conn = db.conn();
            crate::db::collab_frames::update_landed_path(
                &conn,
                &idle.project_id,
                &idle.frame_uuid,
                &path_str,
            )?;
            tracing::info!(
                project_id = %idle.project_id,
                frame_uuid = %idle.frame_uuid,
                path = %path_str,
                "adopt by hash: path recorded for an excluded frame"
            );
        }
        return Ok(AdoptOutcome {
            adopted: Vec::new(),
            matched,
            parked: Vec::new(),
        });
    }

    let mut adopted = Vec::new();
    let mut parked = Vec::new();
    for (i, row) in servable_or_own.iter().enumerate() {
        let is_follower = i != 0;
        let owned_path;
        let landed_path: &Path = if !is_follower {
            path
        } else {
            let dest = root
                .join(".athenaeum")
                .join("adopted")
                .join(&row.project_id)
                .join(&row.frame_uuid)
                .join(&row.file_name);
            if let Some(parent) = dest.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    tracing::warn!(path = %dest.display(), error = %e, "adopt by hash: preparing a follower copy failed");
                    continue;
                }
            }
            if let Err(e) = crate::sync::ingest::link_or_copy(path, &dest, false) {
                tracing::warn!(path = %dest.display(), error = %format!("{e:#}"), "adopt by hash: copying a follower failed");
                continue;
            }
            owned_path = dest;
            &owned_path
        };
        match land_candidate(ctx, node, row, landed_path).await {
            Ok(Landing::Landed { from, to }) => adopted.push(Adopted {
                project_id: row.project_id.clone(),
                frame_uuid: row.frame_uuid.clone(),
                from,
                to,
            }),
            // a parked follower keeps its copy: it is the recorded path now
            Ok(Landing::Parked) => parked.push((row.project_id.clone(), row.frame_uuid.clone())),
            Ok(Landing::Refused) => {
                if is_follower {
                    cleanup_follower_copy(landed_path);
                }
            }
            Err(e) => {
                // Fix round 3, point 4: `?` must not skip the cleanup below —
                // an `Err` from `land_candidate` (e.g. a DB failure mid
                // transaction) leaves the SAME orphaned follower copy a
                // simple `Ok(false)` does.
                if is_follower {
                    cleanup_follower_copy(landed_path);
                }
                return Err(e);
            }
        }
    }
    Ok(AdoptOutcome {
        adopted,
        matched,
        parked,
    })
}

/// Fix round 2, ruling 7 (and round 3, point 4 — the `Err` path too): a
/// follower copy under `.athenaeum/adopted/` that failed to seed, whose
/// content mismatched, or whose landing errored outright is an orphan
/// nothing tracks — removed rather than left behind.
fn cleanup_follower_copy(landed_path: &Path) {
    if let Err(e) = std::fs::remove_file(landed_path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(path = %landed_path.display(), error = %e, "adopt by hash: cleaning up a failed follower copy failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::collab_live::test_support::{
        self, collab_root, my_device, seed_replica_file, signed_in_rig,
    };

    #[tokio::test]
    async fn an_offline_account_device_is_offered_and_a_foreign_device_is_not() {
        let (_t, ctx, hub) = signed_in_rig().await;
        hub.add_device(
            "acc-me",
            "OLD-DEV",
            "old-id",
            "Old laptop",
            Some(chrono::Utc::now() - chrono::Duration::days(9)),
        );
        let offer = replace_offer(&ctx, "OLD-DEV").await.unwrap().unwrap();
        assert_eq!(
            (offer.device_id.as_str(), offer.prompt, offer.propose_retire),
            ("old-id", true, false)
        );
        assert!(replace_offer(&ctx, "SOMEONE-ELSE").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn replacing_retires_rewrites_the_marker_and_adopts_files_by_hash() {
        let (_t, ctx, hub) = signed_in_rig().await;
        let root = collab_root(&ctx);
        hub.add_device("acc-me", "OLD-DEV", "old-id", "Old laptop", None);
        // the old device's landed replica is already in the folder
        let (pid, uuid, path) = seed_replica_file(&ctx, &hub, &root).await;
        // Same physical store `signed_in_rig` already recorded — just
        // "handed over" to OLD-DEV's ownership (a stray store_id here would
        // simulate a genuinely DIFFERENT disk, which is the MarkerMismatch
        // case, not a replace).
        let store_id = read_marker(&root).unwrap().unwrap().store_id;
        write_marker(
            &root,
            &StoreMarker {
                store_id,
                device_id: "OLD-DEV".into(),
            },
        )
        .unwrap();

        let out = replace_device(&ctx, "old-id", &root, &crate::api::PathPolicy::AllowAll)
            .await
            .unwrap();
        assert_eq!(out.adopted, 1);
        assert!(hub.device_retired("old-id"));
        assert_eq!(
            read_marker(&root).unwrap().unwrap().device_id,
            my_device(&ctx).await
        );

        let conn = db(&ctx).unwrap().conn();
        let row = crate::db::collab_frames::get(&conn, &pid, &uuid)
            .unwrap()
            .unwrap();
        assert_eq!(row.local_state, LocalState::Held);
        assert_eq!(
            row.landed_path.as_deref(),
            Some(path.to_string_lossy().as_ref())
        );
        assert_eq!(crate::db::collab_live::outbox_len(&conn, &pid).unwrap(), 1);
        // one add, zero transfer
    }

    /// Critical fix round 1, point 1: the reinstall flow. A plain
    /// `set_collaboration_dir` on a folder whose marker names another
    /// device is refused outright (no scan_root row, nothing recorded) —
    /// `replace_device` on the SAME (device, root) must still succeed:
    /// designate, rewrite the marker to this device, and adopt what was
    /// already there.
    #[tokio::test]
    async fn a_reinstall_replace_designates_and_adopts_after_the_folder_refused_first() {
        let (tmp, ctx, hub) = test_support::signed_in_rig_no_root().await;
        let root = tmp.path().join("Collab");
        std::fs::create_dir_all(&root).unwrap();
        hub.add_device("acc-me", "OLD-DEV", "old-id", "Old laptop", None);
        write_marker(
            &root,
            &StoreMarker {
                store_id: "s1".into(),
                device_id: "OLD-DEV".into(),
            },
        )
        .unwrap();
        let (pid, uuid, path) = seed_replica_file(&ctx, &hub, &root).await;

        let err = crate::api::scan_roots::set_collaboration_dir(
            &ctx,
            root.to_string_lossy().to_string(),
            &crate::api::PathPolicy::AllowAll,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, ApiError::Conflict(ref m) if m.starts_with("collab_other_device")),
            "{err:?}"
        );
        assert_eq!(
            crate::api::scan_roots::get_collaboration_dir(&ctx).unwrap(),
            None
        );

        let out = replace_device(&ctx, "old-id", &root, &crate::api::PathPolicy::AllowAll)
            .await
            .unwrap();
        assert_eq!(out.adopted, 1);
        assert!(hub.device_retired("old-id"));
        assert_eq!(
            crate::api::scan_roots::get_collaboration_dir(&ctx).unwrap(),
            Some(
                crate::test_support::canonical_path(&root)
                    .to_string_lossy()
                    .to_string()
            )
        );
        assert_eq!(
            read_marker(&root).unwrap().unwrap().device_id,
            my_device(&ctx).await
        );

        let conn = db(&ctx).unwrap().conn();
        let row = crate::db::collab_frames::get(&conn, &pid, &uuid)
            .unwrap()
            .unwrap();
        assert_eq!(row.local_state, LocalState::Held);
        // Fix round 2, ruling 5: the walk uses the CANONICAL root, so the
        // recorded landed_path is the canonical spelling of `path`, not
        // necessarily `path`'s own (possibly non-canonical) string form.
        assert_eq!(
            row.landed_path.as_deref(),
            Some(
                crate::test_support::canonical_path(&path)
                    .to_string_lossy()
                    .to_string()
            )
            .as_deref()
        );
    }

    /// Fix round 2, ruling 2 (REPLACES the round-1 test of the same idea):
    /// a `device_id` no longer listed among this account's active devices
    /// refuses outright, with NO hub call — even when it's exactly the
    /// device the on-disk marker still names. Absence from the active list
    /// is no longer treated as "must already be retired by an earlier run
    /// of this same call" (round 1's assumption) — that recovery path is
    /// now [`take_over_collab_folder`] (see the test below).
    #[tokio::test]
    async fn replace_device_refuses_an_unlisted_device_with_no_hub_call() {
        let (tmp, ctx, hub) = test_support::signed_in_rig_no_root().await;
        let root = tmp.path().join("Collab");
        std::fs::create_dir_all(&root).unwrap();
        // "old-id" was never registered at all — never listed, never even a
        // real prior device of this account.
        write_marker(
            &root,
            &StoreMarker {
                store_id: "s1".into(),
                device_id: "OLD-DEV".into(),
            },
        )
        .unwrap();

        let err = replace_device(&ctx, "old-id", &root, &crate::api::PathPolicy::AllowAll)
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::Invalid(_)), "{err:?}");
        assert!(!hub.device_retired("old-id"), "no hub call was ever made");
        assert_eq!(
            read_marker(&root).unwrap().unwrap().device_id,
            "OLD-DEV",
            "the marker is never touched"
        );
        assert_eq!(
            crate::api::scan_roots::get_collaboration_dir(&ctx).unwrap(),
            None
        );
    }

    /// Fix round 2, ruling 3: the recovery path for a folder whose marker
    /// names a device that is NO LONGER listed (an earlier `replace_device`
    /// retired it, then "crashed" before rewriting the marker) is the
    /// explicit, user-confirmed take-over, not a `replace_device` retry.
    #[tokio::test]
    async fn take_over_completes_the_recovery_a_replace_retry_can_no_longer_do() {
        let (tmp, ctx, hub) = test_support::signed_in_rig_no_root().await;
        let root = tmp.path().join("Collab");
        std::fs::create_dir_all(&root).unwrap();
        hub.add_device("acc-me", "OLD-DEV", "old-id", "Old laptop", None);
        write_marker(
            &root,
            &StoreMarker {
                store_id: "s1".into(),
                device_id: "OLD-DEV".into(),
            },
        )
        .unwrap();

        // Simulate an earlier `replace_device` that retired the old device
        // on the hub, then crashed before touching the marker at all.
        crate::api::account::revoke_device_retire(&ctx, "old-id".to_string())
            .await
            .unwrap();
        assert!(hub.device_retired("old-id"));

        // A plain replace retry now refuses (the device is unlisted).
        let err = replace_device(&ctx, "old-id", &root, &crate::api::PathPolicy::AllowAll)
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::Invalid(_)), "{err:?}");

        // Fix round 3, point 3: `take_over_collab_folder` requires a
        // RECORDED `Unknown` refusal naming this exact marker — a plain
        // `set_collaboration_dir` attempt (as the real recovery UI would
        // run first) records exactly that, since OLD-DEV is no longer listed.
        let refusal_err = crate::api::scan_roots::set_collaboration_dir(
            &ctx,
            root.to_string_lossy().to_string(),
            &crate::api::PathPolicy::AllowAll,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(refusal_err, ApiError::Conflict(ref m) if m.starts_with("collab_unknown_device")),
            "{refusal_err:?}"
        );

        // The user-confirmed take-over completes it, no hub call needed.
        let out = take_over_collab_folder(&ctx, &root, &crate::api::PathPolicy::AllowAll, true)
            .await
            .unwrap();
        assert_eq!(out.scanned, 0);
        assert_eq!(
            read_marker(&root).unwrap().unwrap().device_id,
            my_device(&ctx).await
        );
        assert_eq!(
            crate::api::scan_roots::get_collaboration_dir(&ctx).unwrap(),
            Some(
                crate::test_support::canonical_path(&root)
                    .to_string_lossy()
                    .to_string()
            )
        );
    }

    /// Fix round 2, ruling 3: take-over refuses without `confirmed`, and
    /// never touches the marker.
    #[tokio::test]
    async fn take_over_requires_confirmation() {
        let (tmp, ctx, _hub) = test_support::signed_in_rig_no_root().await;
        let root = tmp.path().join("Collab");
        std::fs::create_dir_all(&root).unwrap();
        write_marker(
            &root,
            &StoreMarker {
                store_id: "s1".into(),
                device_id: "GHOST".into(),
            },
        )
        .unwrap();

        let err = take_over_collab_folder(&ctx, &root, &crate::api::PathPolicy::AllowAll, false)
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::Invalid(_)), "{err:?}");
        assert_eq!(
            read_marker(&root).unwrap().unwrap().device_id,
            "GHOST",
            "unconfirmed: nothing touched"
        );
    }

    /// Fix round 2, ruling 4: every local precondition runs BEFORE the hub
    /// retire call — a folder R5 won't let become the Collaboration root
    /// (already monitored, with cataloged files) refuses with no hub call
    /// at all, even though the device is a verified, active, matching one.
    #[tokio::test]
    async fn replace_device_refuses_before_retiring_when_the_folder_cannot_be_designated() {
        let (tmp, ctx, hub) = test_support::signed_in_rig_no_root().await;
        let root = tmp.path().join("Collab");
        std::fs::create_dir_all(&root).unwrap();
        hub.add_device("acc-me", "OLD-DEV", "old-id", "Old laptop", None);
        write_marker(
            &root,
            &StoreMarker {
                store_id: "s1".into(),
                device_id: "OLD-DEV".into(),
            },
        )
        .unwrap();
        // Register it as a monitored 'normal' root WITH a cataloged file —
        // R5 refuses to promote a monitored library folder.
        crate::api::scan_roots::add_scan_root(
            &ctx,
            root.to_string_lossy().to_string(),
            &crate::api::PathPolicy::AllowAll,
            None,
        )
        .unwrap();
        {
            let conn = db(&ctx).unwrap().conn();
            // The scan root was stored in its canonical spelling; the
            // cataloged file's path must share that same prefix for
            // `collab_promotion_allowed`'s prefix predicate to see it.
            let file = crate::test_support::canonical_path(&root)
                .join("M31")
                .join("L_001.fits");
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format)
                 VALUES (?1, 'L_001.fits', 1, '2026-01-01T00:00:00Z', 'FITS')",
                rusqlite::params![file.to_string_lossy()],
            )
            .unwrap();
        }

        let err = replace_device(&ctx, "old-id", &root, &crate::api::PathPolicy::AllowAll)
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::Invalid(_)), "{err:?}");
        assert!(!hub.device_retired("old-id"), "no hub call was ever made");
        assert_eq!(
            read_marker(&root).unwrap().unwrap().device_id,
            "OLD-DEV",
            "the marker is never touched"
        );
    }

    /// Fix round 1, point 5 (re-verified under fix round 2's precondition
    /// ordering): with no iroh node bound, `replace_device` errs BEFORE
    /// retiring anything.
    #[tokio::test]
    async fn replace_device_errs_when_no_store_can_be_mounted() {
        let (tmp, ctx, hub) = test_support::signed_in_rig_no_root().await;
        *ctx.iroh_node.lock().await = None;
        let root = tmp.path().join("Collab");
        std::fs::create_dir_all(&root).unwrap();
        hub.add_device("acc-me", "OLD-DEV", "old-id", "Old laptop", None);
        write_marker(
            &root,
            &StoreMarker {
                store_id: "s1".into(),
                device_id: "OLD-DEV".into(),
            },
        )
        .unwrap();

        let err = replace_device(&ctx, "old-id", &root, &crate::api::PathPolicy::AllowAll)
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::Internal(_)), "{err:?}");
        assert!(!hub.device_retired("old-id"), "no hub call was ever made");
    }

    /// Fix round 1 (folded minor): an `idle` candidate (caps-excluded, not
    /// yet accepted) only gets its path recorded, under the project's disk
    /// lock — never served, its state stays `idle`.
    #[tokio::test]
    async fn adopt_by_hash_records_the_path_of_an_excluded_frame_without_serving_it() {
        let (_t, ctx, hub) = signed_in_rig().await;
        let root = collab_root(&ctx);
        let uuid = "f-idle-1";
        let bytes = b"an idle frame's bytes".to_vec();
        let blake3 = blake3::hash(&bytes).to_hex().to_string();
        let xxh3 = format!("{:016x}", xxhash_rust::xxh3::xxh3_64(&bytes));
        hub.seed_frames(test_support::PID, "acc-me", &[uuid], "published");
        hub.update_frame(test_support::PID, uuid, |f| {
            f.blake3 = blake3;
            f.byte_size = bytes.len() as i64;
            f.xxh3 = xxh3;
            f.accepted = false; // not yet accepted -> `upsert_from_manifest` files this as `idle`
        });
        let view = hub
            .frame(test_support::PID, uuid)
            .expect("frame just seeded");
        {
            let conn = db(&ctx).unwrap().conn();
            crate::db::collab_frames::upsert_from_manifest(&conn, test_support::PID, &view)
                .unwrap();
        }
        let dest_dir = root.join("m31").join("other2");
        std::fs::create_dir_all(&dest_dir).unwrap();
        let path = dest_dir.join(format!("{uuid}.fits"));
        std::fs::write(&path, &bytes).unwrap();

        let conn = db(&ctx).unwrap().conn();
        assert_eq!(
            crate::db::collab_frames::get(&conn, test_support::PID, uuid)
                .unwrap()
                .unwrap()
                .local_state,
            LocalState::Idle,
            "precondition: the manifest row landed as idle"
        );
        drop(conn);

        let node = crate::api::collab_exchange::bound_node(&ctx).await.unwrap();
        let landed = adopt_by_hash(&ctx, &node, &root, &path).await.unwrap();
        assert!(
            landed.is_empty(),
            "an idle-only match is never counted as adopted"
        );

        let conn = db(&ctx).unwrap().conn();
        let row = crate::db::collab_frames::get(&conn, test_support::PID, uuid)
            .unwrap()
            .unwrap();
        assert_eq!(
            row.local_state,
            LocalState::Idle,
            "state stays idle — never served"
        );
        assert_eq!(
            row.landed_path.as_deref(),
            Some(path.to_string_lossy().as_ref()),
            "the path is still recorded, so an un-exclude lands straight on it"
        );
    }

    /// C10 ("identical bytes in two frames or projects — blob ≠ reference",
    /// spec table): a single walked file matching TWO servable frame rows
    /// (a duplicate publish) adopts BOTH, not just the first. The path-local
    /// one (both are in `m31` here, so the alphabetically-first uuid) keeps
    /// the walked path; the other gets its own on-disk copy, since
    /// `landed_path` is a UNIQUE column.
    #[tokio::test]
    async fn adopt_by_hash_adopts_every_matching_candidate() {
        let (_t, ctx, hub) = signed_in_rig().await;
        let root = collab_root(&ctx);
        let bytes = b"identical bytes shared by two frame rows".to_vec();
        let blake3 = blake3::hash(&bytes).to_hex().to_string();
        let xxh3 = format!("{:016x}", xxhash_rust::xxh3::xxh3_64(&bytes));
        for uuid in ["f-dup-a", "f-dup-b"] {
            hub.seed_frames(test_support::PID, "acc-me", &[uuid], "published");
            hub.update_frame(test_support::PID, uuid, |f| {
                f.blake3 = blake3.clone();
                f.byte_size = bytes.len() as i64;
                f.xxh3 = xxh3.clone();
                f.file_name = format!("{uuid}.fits");
            });
            let view = hub
                .frame(test_support::PID, uuid)
                .expect("frame just seeded");
            let conn = db(&ctx).unwrap().conn();
            crate::db::collab_frames::upsert_from_manifest(&conn, test_support::PID, &view)
                .unwrap();
        }
        let dest_dir = root.join("m31").join("other");
        std::fs::create_dir_all(&dest_dir).unwrap();
        let path = dest_dir.join("f-dup-a.fits");
        std::fs::write(&path, &bytes).unwrap();

        let node = crate::api::collab_exchange::bound_node(&ctx).await.unwrap();
        let landed = adopt_by_hash(&ctx, &node, &root, &path).await.unwrap();
        assert_eq!(landed.len(), 2, "{landed:?}");

        let conn = db(&ctx).unwrap().conn();
        let a = crate::db::collab_frames::get(&conn, test_support::PID, "f-dup-a")
            .unwrap()
            .unwrap();
        let b = crate::db::collab_frames::get(&conn, test_support::PID, "f-dup-b")
            .unwrap()
            .unwrap();
        assert_eq!(a.local_state, LocalState::Held);
        assert_eq!(b.local_state, LocalState::Held);
        assert_eq!(
            a.landed_path.as_deref(),
            Some(path.to_string_lossy().as_ref()),
            "the primary keeps the walked path"
        );
        let b_path = b.landed_path.expect("the follower landed somewhere");
        assert_ne!(
            b_path,
            path.to_string_lossy(),
            "the follower gets its own path — landed_path is a UNIQUE column"
        );
        assert_eq!(
            std::fs::read(&b_path).unwrap(),
            bytes,
            "same bytes, hardlinked or copied"
        );
    }

    /// Fix round 3, point 1 (critical — a round-2 test change hid this): a
    /// first-time replace on a root that is ALREADY the designated
    /// Collaboration root, whose on-disk marker's store id disagrees with
    /// what THIS CATALOG recorded for that path, must refuse before the
    /// (irreversible) hub retire call — not let `set_collaboration_dir`'s
    /// own `MarkerMismatch` catch it only afterwards.
    #[tokio::test]
    async fn replace_device_refuses_a_mismatched_recorded_store_id_before_retiring() {
        let (_t, ctx, hub) = signed_in_rig().await;
        let root = collab_root(&ctx);
        hub.add_device("acc-me", "OLD-DEV", "old-id", "Old laptop", None);
        // A genuinely DIFFERENT store id than the one `signed_in_rig`
        // recorded for this path — a different disk, not just a different
        // owning device of the SAME store.
        write_marker(
            &root,
            &StoreMarker {
                store_id: "a-different-store".into(),
                device_id: "OLD-DEV".into(),
            },
        )
        .unwrap();

        let err = replace_device(&ctx, "old-id", &root, &crate::api::PathPolicy::AllowAll)
            .await
            .unwrap_err();
        match err {
            ApiError::Conflict(m) => assert!(m.contains("MarkerMismatch"), "{m}"),
            other => panic!("expected a MarkerMismatch Conflict, got {other:?}"),
        }
        assert!(!hub.device_retired("old-id"), "no hub call was ever made");
        assert_eq!(
            read_marker(&root).unwrap().unwrap(),
            StoreMarker {
                store_id: "a-different-store".into(),
                device_id: "OLD-DEV".into(),
            },
            "the marker is never touched"
        );
    }

    /// Fix round 3, point 1: the same protection for `take_over_collab_folder`
    /// — it must not rewrite a marker whose store id disagrees with what
    /// this catalog already recorded for the path.
    #[tokio::test]
    async fn take_over_refuses_a_mismatched_recorded_store_id() {
        let (_t, ctx, _hub) = signed_in_rig().await;
        let root = collab_root(&ctx);
        write_marker(
            &root,
            &StoreMarker {
                store_id: "a-different-store".into(),
                device_id: "GHOST".into(),
            },
        )
        .unwrap();
        // Record the `Unknown` refusal take-over's own gate (point 3) requires.
        let refusal_err = crate::api::scan_roots::set_collaboration_dir(
            &ctx,
            root.to_string_lossy().to_string(),
            &crate::api::PathPolicy::AllowAll,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(refusal_err, ApiError::Conflict(ref m) if m.starts_with("collab_unknown_device")),
            "{refusal_err:?}"
        );

        let err = take_over_collab_folder(&ctx, &root, &crate::api::PathPolicy::AllowAll, true)
            .await
            .unwrap_err();
        match err {
            ApiError::Conflict(m) => assert!(m.contains("MarkerMismatch"), "{m}"),
            other => panic!("expected a MarkerMismatch Conflict, got {other:?}"),
        }
        assert_eq!(
            read_marker(&root).unwrap().unwrap(),
            StoreMarker {
                store_id: "a-different-store".into(),
                device_id: "GHOST".into(),
            },
            "the marker is never touched"
        );
    }

    /// Fix round 3, point 2: the policy gate runs BEFORE any filesystem
    /// probe or hub call. `root` here never exists on disk, so if
    /// `root.is_dir()`/`read_marker` ran first the error would be
    /// `Invalid("... is not an existing folder")`, not `Forbidden` — and no
    /// request would reach the fake hub either way.
    #[tokio::test]
    async fn replace_device_refuses_a_root_outside_policy_before_any_probe_or_hub_call() {
        let (tmp, ctx, hub) = test_support::signed_in_rig_no_root().await;
        let outside = tmp.path().join("Outside");
        let policy = crate::api::PathPolicy::AllowedRoots(vec![tmp.path().join("Allowed")]);
        let before = hub.server.received_requests().await.unwrap().len();

        let err = replace_device(&ctx, "old-id", &outside, &policy)
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::Forbidden(_)), "{err:?}");

        let after = hub.server.received_requests().await.unwrap().len();
        assert_eq!(before, after, "no hub call was ever made");
    }

    /// Fix round 3, point 2: the same ordering for `take_over_collab_folder`.
    #[tokio::test]
    async fn take_over_refuses_a_root_outside_policy_before_any_probe() {
        let (tmp, ctx, _hub) = test_support::signed_in_rig_no_root().await;
        let outside = tmp.path().join("Outside");
        let policy = crate::api::PathPolicy::AllowedRoots(vec![tmp.path().join("Allowed")]);

        let err = take_over_collab_folder(&ctx, &outside, &policy, true)
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::Forbidden(_)), "{err:?}");
    }

    /// Fix round 3, point 3 (controller ruling): a device the account still
    /// lists as active can never be taken over, even with `confirmed: true`
    /// — no `Unknown` refusal was ever recorded for this marker, so
    /// `take_over_collab_folder` refuses regardless of the account's
    /// CURRENT (fresh, hub-free) device list.
    #[tokio::test]
    async fn take_over_refuses_an_active_devices_folder_even_when_confirmed() {
        let (tmp, ctx, hub) = test_support::signed_in_rig_no_root().await;
        let root = tmp.path().join("Collab");
        std::fs::create_dir_all(&root).unwrap();
        hub.add_device("acc-me", "OLD-DEV", "old-id", "Old laptop", None);
        write_marker(
            &root,
            &StoreMarker {
                store_id: "s1".into(),
                device_id: "OLD-DEV".into(),
            },
        )
        .unwrap();

        // No `set_collaboration_dir` attempt was ever made — no refusal of
        // any kind is recorded for this folder.
        let err = take_over_collab_folder(&ctx, &root, &crate::api::PathPolicy::AllowAll, true)
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::Invalid(_)), "{err:?}");
        assert_eq!(
            read_marker(&root).unwrap().unwrap().device_id,
            "OLD-DEV",
            "never touched"
        );
        assert_eq!(
            crate::api::scan_roots::get_collaboration_dir(&ctx).unwrap(),
            None
        );
    }

    /// Fix round 3, point 4: `land_candidate(...).await?` must not skip the
    /// follower cleanup on an `Err` OR an `Ok(false)` (mismatch) outcome.
    /// Forcing a genuine `Err` from `land_candidate` needs deeper store
    /// mocking than this suite has, so this exercises the `Ok(false)`
    /// (hash-mismatch) branch — it shares the exact same
    /// `cleanup_follower_copy` call the `Err` branch now also makes.
    #[tokio::test]
    async fn adopt_by_hash_cleans_up_a_failed_followers_copy() {
        let (_t, ctx, hub) = signed_in_rig().await;
        let root = collab_root(&ctx);
        let bytes = b"identical bytes, but one manifest lies about its hash".to_vec();
        let real_blake3 = blake3::hash(&bytes).to_hex().to_string();
        let xxh3 = format!("{:016x}", xxhash_rust::xxh3::xxh3_64(&bytes));
        // f-dup-a: correct blake3 (the primary, path-local — lands fine).
        // f-dup-b: WRONG blake3 (the follower — its copy must be cleaned up).
        for (uuid, blake3_val) in [
            ("f-dup-a", real_blake3.clone()),
            ("f-dup-b", "b".repeat(64)),
        ] {
            hub.seed_frames(test_support::PID, "acc-me", &[uuid], "published");
            hub.update_frame(test_support::PID, uuid, |f| {
                f.blake3 = blake3_val;
                f.byte_size = bytes.len() as i64;
                f.xxh3 = xxh3.clone();
                f.file_name = format!("{uuid}.fits");
            });
            let view = hub
                .frame(test_support::PID, uuid)
                .expect("frame just seeded");
            let conn = db(&ctx).unwrap().conn();
            crate::db::collab_frames::upsert_from_manifest(&conn, test_support::PID, &view)
                .unwrap();
        }
        let dest_dir = root.join("m31").join("other");
        std::fs::create_dir_all(&dest_dir).unwrap();
        let path = dest_dir.join("f-dup-a.fits");
        std::fs::write(&path, &bytes).unwrap();

        let node = crate::api::collab_exchange::bound_node(&ctx).await.unwrap();
        let landed = adopt_by_hash(&ctx, &node, &root, &path).await.unwrap();
        assert_eq!(landed.len(), 1, "{landed:?}"); // only f-dup-a lands

        let follower_copy = root
            .join(".athenaeum")
            .join("adopted")
            .join(test_support::PID)
            .join("f-dup-b")
            .join("f-dup-b.fits");
        assert!(
            !follower_copy.exists(),
            "the failed follower copy is cleaned up, not left behind"
        );

        let conn = db(&ctx).unwrap().conn();
        let b = crate::db::collab_frames::get(&conn, test_support::PID, "f-dup-b")
            .unwrap()
            .unwrap();
        assert_eq!(b.local_state, LocalState::Wanted, "never landed");
        assert_eq!(b.landed_path, None);
    }

    /// Task 11 (ledger ruling): `land_candidate` writes through the storage
    /// engine's fenced transaction — a row that moved on after the caller
    /// read it (here: a manifest version bump) is never turned `held` from
    /// the stale view; the current row adopts normally.
    #[tokio::test]
    async fn land_candidate_never_records_a_row_that_moved_on() {
        let (_t, ctx, hub) = signed_in_rig().await;
        let root = collab_root(&ctx);
        let (pid, uuid, path) = seed_replica_file(&ctx, &hub, &root).await;
        let node = crate::api::collab_exchange::bound_node(&ctx).await.unwrap();
        let stale = crate::db::collab_frames::get(&db(&ctx).unwrap().conn(), &pid, &uuid)
            .unwrap()
            .unwrap();
        assert_eq!(stale.local_state, LocalState::Wanted);
        db(&ctx)
            .unwrap()
            .conn()
            .execute(
                "UPDATE project_frames_local SET content_version = content_version + 1
                 WHERE project_id = ?1 AND frame_uuid = ?2",
                rusqlite::params![pid, uuid],
            )
            .unwrap();
        assert_eq!(
            land_candidate(&ctx, &node, &stale, &path).await.unwrap(),
            Landing::Refused
        );
        let row = crate::db::collab_frames::get(&db(&ctx).unwrap().conn(), &pid, &uuid)
            .unwrap()
            .unwrap();
        assert_eq!(
            row.local_state,
            LocalState::Wanted,
            "not held from a stale view"
        );
        assert_eq!(row.landed_path, None);
        assert_eq!(row.content_version, stale.content_version + 1);

        // the current row (same bytes, so it still hashes to its blake3) adopts
        assert!(matches!(
            land_candidate(&ctx, &node, &row, &path).await.unwrap(),
            Landing::Landed {
                to: LocalState::Held,
                ..
            }
        ));
    }
}

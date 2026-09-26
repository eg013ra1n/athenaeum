//! The storage marker's device-facing half (spec §9.5, plan P22/P29): is the
//! marker's device one of this account's OWN devices (offer a replace,
//! never a foreign store), and the device-replace core — retire the old
//! device, rewrite the marker to name this one, and re-adopt everything
//! already sitting in the folder by content hash (the files never move).

use std::path::Path;

use crate::api::{db, ApiError};
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

/// Replace `device_id` (a hub device id, e.g. from a [`ReplaceOffer`]) as the
/// owner of `root` — a Collaboration root that is PENDING designation (the
/// reinstall flow: a plain `set_collaboration_dir` on this path would refuse
/// with `collab_other_device`, since the marker still names the device being
/// replaced) or already designated (a re-run after a partial failure).
///
/// Fix round 1, point 1 (critical): every check that does not itself change
/// anything runs BEFORE the retire call, which is the one step this
/// function cannot cleanly undo:
///
/// 1. `root` must exist as a folder.
/// 2. the on-disk marker must exist. If it ALREADY names this device, a
///    prior run got this far — the retire/rewrite steps are skipped and
///    only designate-and-adopt runs (idempotent re-run).
/// 3. otherwise, the marker must name the device being replaced — checked
///    against the hub's still-active device list when possible; a device
///    the hub no longer lists (already retired by an earlier, partial run
///    of THIS SAME replace) cannot be re-checked and is accepted as-is.
/// 4. the folder must be writable — the marker rewrite below must be able
///    to land, checked before, not after, retiring the old device.
///
/// Only once all of that holds does it retire the old device on the hub
/// (spec §9.5 — a retired device is never re-offered; a `404` for an
/// already-retired device is treated as done, not a failure), rewrite the
/// marker to name this device under the SAME store id, designate `root` as
/// the Collaboration root (its own storage check now passes, since the
/// marker already names this device) and mount it directly, then walk the
/// folder re-adopting every file already there by content hash.
pub async fn replace_device(
    ctx: &ServiceContext,
    device_id: &str,
    root: &Path,
) -> Result<ReplaceOutcome, ApiError> {
    if !root.is_dir() {
        tracing::warn!(path = %root.display(), "device replace: the folder does not exist");
        return Err(ApiError::Invalid(format!(
            "{} is not an existing folder",
            root.display()
        )));
    }

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
        if let Some(dev) = crate::api::account::list_devices(ctx)
            .await?
            .into_iter()
            .find(|d| d.id == device_id)
        {
            if dev.pubkey != marker.device_id {
                tracing::warn!(
                    path = %root.display(),
                    device_id,
                    marker_device = %marker.device_id,
                    "device replace refused: the marker names a different device"
                );
                return Err(ApiError::Invalid(
                    "the storage marker names a different device than the one being replaced"
                        .to_string(),
                ));
            }
        }

        if !writable(root) {
            return Err(ApiError::Invalid(format!(
                "{} is not writable",
                root.display()
            )));
        }

        match crate::api::account::revoke_device_retire(ctx, device_id.to_string()).await {
            Ok(()) => {}
            Err(ApiError::Invalid(m)) if m.to_lowercase().contains("no such device") => {
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
        {
            let db = db(ctx)?;
            let conn = db.conn();
            crate::db::collab_live::record_store_marker(
                &conn,
                &new_marker,
                &root.to_string_lossy(),
            )?;
        }
    }

    // Designate (or confirm) `root` as the Collaboration root and mount it
    // DIRECTLY (`set_collaboration_dir` mounts through `mount_collab_store`,
    // unconditional — never the rate-limited `ensure_collab_store` lazy
    // path, which could still be sitting on the pre-replace OtherDevice
    // latch for up to 60s). The marker now names this device, so the
    // designation's own storage check passes.
    crate::api::scan_roots::set_collaboration_dir(
        ctx,
        root.to_string_lossy().to_string(),
        &crate::api::PathPolicy::AllowAll,
    )
    .await?;

    let node = crate::api::collab_exchange::bound_node(ctx)
        .await
        .ok_or_else(|| {
            tracing::error!(path = %root.display(), "device replace: no iroh node bound; cannot adopt");
            ApiError::Internal("no collaboration store available to adopt into".to_string())
        })?;
    if node.collab_store().is_none() {
        tracing::error!(path = %root.display(), "device replace: the collaboration store is not mounted; cannot adopt");
        return Err(ApiError::Internal(
            "the collaboration store is not mounted".to_string(),
        ));
    }

    let mut scanned = 0usize;
    let mut adopted = 0usize;
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| e.file_name() != ".athenaeum")
    {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(path = %root.display(), error = %e, "device replace: walk entry failed");
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
        match adopt_by_hash(ctx, &node, root, path).await {
            Ok(landed) => adopted += landed.len(),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "device replace: adopt by hash failed")
            }
        }
    }

    tracing::info!(
        device_id,
        scanned,
        count = adopted,
        "collaboration folder re-adopted after a device replace"
    );
    Ok(ReplaceOutcome { scanned, adopted })
}

/// Seed `landed_path`'s bytes into the store for `row` (reference import —
/// the file never moves), verify the BLAKE3 the store reads back against
/// the manifest, and — once verified — record `landed_path` under one
/// transaction. A mismatch unseeds the frame again (fix round 1: never
/// leaves a wrongly-tagged blob pinned) and reports no adoption.
async fn land_candidate(
    ctx: &ServiceContext,
    node: &SharedIrohNode,
    row: &LocalFrameRow,
    landed_path: &Path,
) -> Result<bool, ApiError> {
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
            return Ok(false);
        }
    };
    if hash.to_string() != row.blake3 {
        tracing::warn!(
            project_id = %row.project_id,
            frame_uuid = %row.frame_uuid,
            path = %landed_str,
            blake3 = %row.blake3,
            got_blake3 = %hash,
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
        return Ok(false);
    }

    let target_state = if row.origin == FrameOrigin::Own {
        LocalState::OwnHeld
    } else {
        LocalState::Held
    };
    let lock = crate::api::collab_exchange::project_disk_lock(ctx, &row.project_id)?;
    let _guard = lock.lock().await;
    let db = db(ctx)?;
    let conn = db.conn();
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    crate::db::collab_frames::update_landed_path(
        &tx,
        &row.project_id,
        &row.frame_uuid,
        &landed_str,
    )?;
    if let Ok(meta) = std::fs::metadata(landed_path) {
        crate::db::collab_frames::set_size_mtime_seen(
            &tx,
            &row.project_id,
            &row.frame_uuid,
            &crate::api::collab_exchange::size_mtime_from(&meta),
        )?;
    }
    crate::db::collab_frames::set_local_state(&tx, &row.project_id, &row.frame_uuid, target_state)?;
    tx.commit().map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(true)
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
    let size = match tokio::fs::metadata(path).await {
        Ok(m) => m.len() as i64,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "adopt by hash: stat failed");
            return Ok(Vec::new());
        }
    };
    let xxh3 = match crate::api::collab_exchange::xxh3_on_blocking(path).await {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %format!("{e:#}"), "adopt by hash: hash failed");
            return Ok(Vec::new());
        }
    };
    let path_str = path.to_string_lossy().to_string();
    let local_folder = path
        .strip_prefix(root)
        .ok()
        .and_then(|rel| rel.components().next())
        .map(|c| c.as_os_str().to_string_lossy().to_string());

    let db = db(ctx)?;
    let (servable_or_own, idle) = {
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
                )
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
        (servable_or_own, idle)
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
        return Ok(Vec::new());
    }

    let mut adopted = Vec::new();
    for (i, row) in servable_or_own.iter().enumerate() {
        let owned_path;
        let landed_path: &Path = if i == 0 {
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
        if land_candidate(ctx, node, row, landed_path).await? {
            adopted.push((row.project_id.clone(), row.frame_uuid.clone()));
        }
    }
    Ok(adopted)
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
        write_marker(
            &root,
            &StoreMarker {
                store_id: "s1".into(),
                device_id: "OLD-DEV".into(),
            },
        )
        .unwrap();

        let out = replace_device(&ctx, "old-id", &root).await.unwrap();
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

        let out = replace_device(&ctx, "old-id", &root).await.unwrap();
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
        assert_eq!(
            row.landed_path.as_deref(),
            Some(path.to_string_lossy().as_ref())
        );
    }

    /// Critical fix round 1, point 1: a re-run after a partial failure. An
    /// earlier attempt retired the old device on the hub and then "crashed"
    /// before touching the marker at all — the retry must not error just
    /// because the hub now 404s the retire, and must still finish
    /// (designate + rewrite the marker).
    #[tokio::test]
    async fn replace_device_completes_on_a_retry_after_a_partial_failure() {
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

        crate::api::account::revoke_device_retire(&ctx, "old-id".to_string())
            .await
            .unwrap();
        assert!(hub.device_retired("old-id"));

        let out = replace_device(&ctx, "old-id", &root).await.unwrap();
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

    /// Fix round 1, point 5: with no store to adopt into, `replace_device`
    /// errs rather than silently reporting `adopted: 0`.
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

        let err = replace_device(&ctx, "old-id", &root).await.unwrap_err();
        assert!(matches!(err, ApiError::Internal(_)), "{err:?}");
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
}

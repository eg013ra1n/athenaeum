//! The storage marker's device-facing half (spec §9.5, plan P22/P29): is the
//! marker's device one of this account's OWN devices (offer a replace,
//! never a foreign store), and the device-replace core — retire the old
//! device, rewrite the marker to name this one, and re-adopt everything
//! already sitting in the folder by content hash (the files never move).

use std::path::Path;

use crate::api::{db, ApiError};
use crate::collab::storage::marker::{offer_flags, read_marker, write_marker, StoreMarker};
use crate::db::collab_frames::LocalState;
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
/// owner of the designated Collaboration root: retire it on the hub (spec
/// §9.5 — a retired device is never re-offered), rewrite the on-disk marker
/// to name THIS device while keeping the store id (this IS that store, just
/// under new management), record it, mount the store, and walk the root
/// re-adopting every file already there by content hash.
pub async fn replace_device(
    ctx: &ServiceContext,
    device_id: &str,
) -> Result<ReplaceOutcome, ApiError> {
    crate::api::account::revoke_device_retire(ctx, device_id.to_string()).await?;

    let root = crate::api::collab_exchange::require_collaboration_root(ctx)?;
    let me = crate::api::account::own_device_id(ctx)?;
    let store_id = match read_marker(&root) {
        Ok(Some(m)) => m.store_id,
        Ok(None) => uuid::Uuid::new_v4().to_string(),
        Err(e) => {
            tracing::error!(path = %root.display(), error = %e, "device replace: reading the storage marker failed");
            return Err(ApiError::Internal(format!("read storage marker: {e}")));
        }
    };
    let marker = StoreMarker {
        store_id,
        device_id: me,
    };
    write_marker(&root, &marker).map_err(|e| {
        tracing::error!(path = %root.display(), error = %e, "device replace: writing the storage marker failed");
        ApiError::Internal(format!("write storage marker: {e}"))
    })?;
    {
        let db = db(ctx)?;
        let conn = db.conn();
        crate::db::collab_live::record_store_marker(&conn, &marker, &root.to_string_lossy())?;
    }

    if crate::api::collab_exchange::ensure_collab_store(ctx)
        .await
        .is_none()
    {
        tracing::warn!(path = %root.display(), "device replace: the collaboration store did not mount after the marker rewrite");
    }

    let mut scanned = 0usize;
    let mut adopted = 0usize;
    match crate::api::collab_exchange::bound_node(ctx).await {
        Some(node) => {
            for entry in walkdir::WalkDir::new(&root)
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
                match adopt_by_hash(ctx, &node, path).await {
                    Ok(Some(_)) => adopted += 1,
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!(path = %path.display(), error = %e, "device replace: adopt by hash failed")
                    }
                }
            }
        }
        None => {
            tracing::warn!(path = %root.display(), "device replace: no iroh node bound; re-adoption skipped")
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

/// Match a file already sitting in the Collaboration root, by content hash,
/// to a cached project frame — the device-replace re-adoption step (and,
/// later tasks, the same disk-truth "moved/duplicate" leg per frame). Scoped
/// by `(size, xxh3)` across every project this device caches
/// ([`crate::db::collab_frames::project_ids`], mirroring the scanner's own
/// reconciliation). A servable-or-own candidate is seeded (reference import
/// — the file never moves) and, once the BLAKE3 the store reads back
/// confirms it is really that content, landed under one transaction. An
/// `idle` candidate (caps-excluded, not yet accepted/published) only gets
/// its path recorded — never served, its state stays `idle` — and does not
/// count as adopted. Returns `Some((project_id, frame_uuid))` on a servable
/// adoption, `None` otherwise (nothing matched, an idle-only match, or the
/// content did not verify).
pub(crate) async fn adopt_by_hash(
    ctx: &ServiceContext,
    node: &SharedIrohNode,
    path: &Path,
) -> Result<Option<(String, String)>, ApiError> {
    let size = match tokio::fs::metadata(path).await {
        Ok(m) => m.len() as i64,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "adopt by hash: stat failed");
            return Ok(None);
        }
    };
    let xxh3 = match crate::api::collab_exchange::xxh3_on_blocking(path).await {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %format!("{e:#}"), "adopt by hash: hash failed");
            return Ok(None);
        }
    };
    let path_str = path.to_string_lossy().to_string();

    let db = db(ctx)?;
    let row = {
        let conn = db.conn();
        let mut candidates = Vec::new();
        for project_id in crate::db::collab_frames::project_ids(&conn)? {
            candidates.extend(
                crate::db::collab_frames::find_by_project_and_xxh3(&conn, &project_id, &xxh3)?
                    .into_iter()
                    .filter(|r| r.byte_size == size),
            );
        }
        let servable_or_own = candidates.iter().find(|r| {
            matches!(
                r.local_state,
                LocalState::Wanted
                    | LocalState::Missing
                    | LocalState::AwaitingChoice
                    | LocalState::NotKept
                    | LocalState::OwnMissing
            )
        });
        match servable_or_own {
            Some(row) => Some(row.clone()),
            None => {
                if let Some(idle) = candidates
                    .into_iter()
                    .find(|r| r.local_state == LocalState::Idle)
                {
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
                None
            }
        }
    };
    let Some(row) = row else {
        return Ok(None);
    };

    let hash = match node
        .seed_project_frame(&row.project_id, &row.frame_uuid, row.content_version, path)
        .await
    {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(
                project_id = %row.project_id,
                frame_uuid = %row.frame_uuid,
                path = %path_str,
                error = %format!("{e:#}"),
                "adopt by hash: seed failed"
            );
            return Ok(None);
        }
    };
    if hash.to_string() != row.blake3 {
        tracing::warn!(
            project_id = %row.project_id,
            frame_uuid = %row.frame_uuid,
            path = %path_str,
            expected = %row.blake3,
            got = %hash.to_string(),
            "adopt by hash: content does not match the manifest; skipped"
        );
        return Ok(None);
    }

    let target_state = if row.origin == crate::db::collab_frames::FrameOrigin::Own {
        LocalState::OwnHeld
    } else {
        LocalState::Held
    };
    let lock = crate::api::collab_exchange::project_disk_lock(ctx, &row.project_id)?;
    let _guard = lock.lock().await;
    {
        let conn = db.conn();
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        crate::db::collab_frames::update_landed_path(
            &tx,
            &row.project_id,
            &row.frame_uuid,
            &path_str,
        )?;
        if let Ok(meta) = std::fs::metadata(path) {
            crate::db::collab_frames::set_size_mtime_seen(
                &tx,
                &row.project_id,
                &row.frame_uuid,
                &crate::api::collab_exchange::size_mtime_from(&meta),
            )?;
        }
        crate::db::collab_frames::set_local_state(
            &tx,
            &row.project_id,
            &row.frame_uuid,
            target_state,
        )?;
        tx.commit().map_err(|e| ApiError::Internal(e.to_string()))?;
    }
    Ok(Some((row.project_id, row.frame_uuid)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::collab_live::test_support::{
        collab_root, my_device, seed_replica_file, signed_in_rig,
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

        let out = replace_device(&ctx, "old-id").await.unwrap();
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
}

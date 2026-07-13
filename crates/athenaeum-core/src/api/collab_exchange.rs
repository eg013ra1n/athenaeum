//! Holder-side collaboration package exchange — request-to-serve (Stage II
//! collaboration, slice 4, task 6).
//!
//! When a project member asks us (a holder) to serve a package
//! ([`TransportEvent::ProjectRequestReceived`](crate::sharing::types::TransportEvent),
//! wired at the receiver in [`crate::api::sync`]), this module answers:
//!
//! 1. [`handle_project_request`] authorizes the requester
//!    ([`may_serve_package`](crate::collab::authz::may_serve_package) — a
//!    published package to any `send_receive` member or the coordinator, a still
//!    pending one only to the coordinator, Д1), reconstructs a servable directory,
//!    and enqueues an explicit-target send back to the requester through a
//!    DEDICATED collab sender map. Any authorization failure is a **silent**
//!    (warn-logged) drop — cross-account, no error is sent on the wire.
//! 2. [`reconstruct_serve_dir`] rebuilds a byte-identical package directory: a
//!    package I published (`origin='mine'`) returns its retained publication dir
//!    as-is; one I fully received (`origin='received'`) is materialized under
//!    `<sync_dir>/collab_serve/<package_id>/` from the retained manifest bytes +
//!    the landed contribution payloads (hard-linked, copy fallback), idempotently.
//! 3. [`CollabCleanupSink`] cleans a reconstructed `collab_serve` dir once the
//!    serve reaches a terminal state, while NEVER touching a retained
//!    `collab_pub` publication (Д4).
//! 4. [`ensure_collab_sender_engine`] mirrors [`crate::api::sync::ensure_sender_engine`]
//!    but binds a dedicated `<sync_dir>/blobs_collab` blob store (audit m7: a
//!    second `FsStore` over the personal-sync `blobs_out` risks the redb lock and
//!    the startup tag-sweep) and spawns each engine with the [`CollabCleanupSink`]
//!    + a host emitter so a project serve still surfaces `sync-progress` /
//!    `sync-finished`.
//!
//! Ungated (no render gate): depends only on `db`, `sync`, `sharing`, `collab`,
//! `package`, so it compiles in the headless (`--no-default-features`) build.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};

use crate::api::{db, ApiError};
use crate::db::collab_exchange::ContributionRow;
use crate::events::ProgressEmitter;
use crate::services::ServiceContext;
use crate::sharing::types::NodeId;
use crate::sharing::SharingTransport;
use crate::sync::{
    node_id_hex, pairing, CatalogSyncStore, PackageCleanupSink, StartedSender, SyncEngine,
    SyncEngineHandle, SyncSenderRuntime, SyncStore,
};

/// Rebuild a servable package directory for a locally-held package (`package_id`
/// is the HUB package uuid — the `project_packages` row key).
///
/// - `origin='mine'` ⇒ returns the retained publication dir (`local_dir`) as-is;
///   it already holds a byte-identical manifest + payloads.
/// - `origin='received'` (or any other locally-held origin) ⇒ materializes
///   `<sync_dir>/collab_serve/<package_id>/`: `manifest.ndjson` is written
///   byte-exact from the retained `manifest_ndjson`, and each contribution's
///   payload is hard-linked from its `landed_path` to its `rel_path` (a byte copy
///   is the fallback when hard-linking is impossible, e.g. a cross-device landing
///   root). Idempotent — a second call re-writes the manifest and skips payloads
///   already present.
pub fn reconstruct_serve_dir(
    conn: &rusqlite::Connection,
    sync_dir: &Path,
    package_id: &str,
) -> Result<PathBuf> {
    let pkg = crate::db::collab_exchange::get_package(conn, package_id)?
        .ok_or_else(|| anyhow!("collab package {package_id} not found for serving"))?;

    // A package I published: the retained publication dir is servable as-is (Д2).
    if pkg.origin == "mine" {
        let dir = pkg
            .local_dir
            .as_deref()
            .ok_or_else(|| anyhow!("own collab package {package_id} has no retained local_dir"))?;
        return Ok(PathBuf::from(dir));
    }

    // A package I received: materialize a fresh serve dir from the retained
    // manifest bytes + landed contributions. Guard the hub id (ultimately
    // peer-minted, even if it survived a prior validate_package_id) before it is
    // used as a path segment (C1).
    crate::package::validate_package_id(package_id)
        .with_context(|| format!("reject unsafe collab package id {package_id}"))?;
    let manifest_bytes = pkg.manifest_ndjson.as_deref().ok_or_else(|| {
        anyhow!("collab package {package_id} has no retained manifest to re-serve")
    })?;
    let contributions = crate::db::collab_exchange::contributions_for_package(conn, package_id)?;
    let serve_dir = sync_dir.join("collab_serve").join(package_id);
    materialize_serve_dir(&serve_dir, manifest_bytes, &contributions)
        .with_context(|| format!("reconstruct collab serve dir for {package_id}"))?;
    tracing::info!(
        package_id,
        count = contributions.len(),
        path = %serve_dir.display(),
        "collab serve dir reconstructed"
    );
    Ok(serve_dir)
}

/// Materialize `serve_dir`: `manifest.ndjson` byte-exact from `manifest_bytes`,
/// then each contribution's payload at its `rel_path`. Idempotent.
fn materialize_serve_dir(
    serve_dir: &Path,
    manifest_bytes: &[u8],
    contributions: &[ContributionRow],
) -> Result<()> {
    std::fs::create_dir_all(serve_dir)
        .with_context(|| format!("create collab serve dir {}", serve_dir.display()))?;
    // The manifest is written byte-exact so a re-serve is byte-identical to the
    // original (Д2). Overwriting with the same bytes keeps the second call idempotent.
    let manifest_path = serve_dir.join(crate::package::MANIFEST_FILENAME);
    std::fs::write(&manifest_path, manifest_bytes)
        .with_context(|| format!("write serve manifest {}", manifest_path.display()))?;

    for c in contributions {
        // `rel_path` originated in a peer's manifest — guard before joining (L1).
        crate::package::validate_rel_path(&c.rel_path)
            .with_context(|| format!("reject unsafe contribution rel_path {}", c.rel_path))?;
        let dest = serve_dir.join(&c.rel_path);
        if dest.exists() {
            // Idempotent second call: the payload is already materialized.
            continue;
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create serve payload dir {}", parent.display()))?;
        }
        let src = Path::new(&c.landed_path);
        // Hard-link the landed copy into the serve dir — no second full copy of the
        // frame. Fall back to a byte copy when hard-linking is impossible (a
        // cross-device landing root, EXDEV, or any other link error).
        if let Err(e) = std::fs::hard_link(src, &dest) {
            tracing::debug!(
                src = %src.display(),
                dest = %dest.display(),
                error = %e,
                "collab serve: hard link failed; copying payload instead"
            );
            std::fs::copy(src, &dest).with_context(|| {
                format!("copy serve payload {} -> {}", src.display(), dest.display())
            })?;
        }
    }
    Ok(())
}

/// Terminal-cleanup sink for the collab sender map (Д4). A reconstructed serve
/// dir (`<sync_dir>/collab_serve/<package_id>`) is temporary and must be removed
/// once the serve reaches a terminal state; a retained publication
/// (`<sync_dir>/collab_pub/<package_id>`, an `origin='mine'` `local_dir`) MUST
/// survive so it can be re-served again — this sink never deletes it.
///
/// The discriminator is the parent directory name: only a dir whose parent is
/// exactly `collab_serve` is cleaned. Idempotent and cheap (called on the
/// synchronous engine worker at every terminal).
pub struct CollabCleanupSink;

impl PackageCleanupSink for CollabCleanupSink {
    fn on_terminal(&self, dir: &Path) {
        let under_serve = dir
            .parent()
            .and_then(|p| p.file_name())
            .is_some_and(|n| n == std::ffi::OsStr::new("collab_serve"));
        if !under_serve {
            tracing::debug!(
                path = %dir.display(),
                "collab cleanup: retained dir left in place (not a collab_serve dir)"
            );
            return;
        }
        match std::fs::remove_dir_all(dir) {
            Ok(()) => tracing::info!(path = %dir.display(), "collab serve dir cleaned"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(path = %dir.display(), error = %e, "collab serve dir cleanup failed")
            }
        }
    }
}

/// Authorize + reconstruct the servable dir for a request, returning `Some(dir)`
/// when the serve may proceed and `None` (after a `warn!`) on any silent refusal.
/// The pure decision core of [`handle_project_request`] — no transport, no
/// `.await` — so it is exercised hermetically.
///
/// Refusals (all `Ok(None)`, warn-logged, cross-account so nothing goes back on
/// the wire): unknown package row; a package that is neither mine nor fully
/// received (`local_status != 'complete'`); a request naming a project other than
/// the package's own; a requester [`may_serve_package`](crate::collab::authz::may_serve_package)
/// refuses (a pending package ⇒ coordinator only, a published one ⇒ `send_receive`
/// member or coordinator).
fn authorize_and_reconstruct_serve(
    conn: &rusqlite::Connection,
    sync_dir: &Path,
    from: &NodeId,
    project_id: &str,
    package_id: &str,
) -> Result<Option<PathBuf>> {
    let Some(pkg) = crate::db::collab_exchange::get_package(conn, package_id)? else {
        tracing::warn!(
            project_id,
            package_id,
            from = %node_id_hex(from),
            "project serve refused: unknown package"
        );
        return Ok(None);
    };

    // Holdable = a package I published, or one I fully received (so I retained the
    // manifest bytes + landed the payloads to re-serve them).
    if pkg.origin != "mine" && pkg.local_status != "complete" {
        tracing::warn!(
            project_id,
            package_id,
            origin = %pkg.origin,
            local_status = %pkg.local_status,
            "project serve refused: package not locally complete"
        );
        return Ok(None);
    }

    // The request must name the package's OWN project — a coordinator of one
    // project can't pull another project's package by claiming its own id.
    if pkg.project_id != project_id {
        tracing::warn!(
            request_project = project_id,
            package_project = %pkg.project_id,
            package_id,
            "project serve refused: project mismatch"
        );
        return Ok(None);
    }

    // Authorization is against the package's authoritative project id. A still
    // `pending` package (hub has not published it) may be served ONLY to the
    // coordinator (they decide it); a published one to any `send_receive` member
    // or the coordinator.
    let pending = pkg.state == "pending";
    if !crate::collab::authz::may_serve_package(conn, &pkg.project_id, pending, from) {
        tracing::warn!(
            project_id = %pkg.project_id,
            package_id,
            pending,
            from = %node_id_hex(from),
            "project serve refused: requester not authorized to be served"
        );
        return Ok(None);
    }

    let dir = reconstruct_serve_dir(conn, sync_dir, package_id)?;
    Ok(Some(dir))
}

/// Holder side of Д1: authorize + reconstruct + enqueue an explicit-target serve
/// of `package_id` back to `from` through the collab sender map. `project_id` /
/// `package_id` come off the wire ([`TransportEvent::ProjectRequestReceived`](crate::sharing::types::TransportEvent),
/// where `package_id` is the HUB uuid). An authorization/eligibility failure is a
/// silent (warn-logged) drop — cross-account, so no error is returned to the
/// requester.
pub async fn handle_project_request(
    ctx: &ServiceContext,
    sender: &SyncSenderRuntime,
    from: NodeId,
    project_id: String,
    package_id: String,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Result<()> {
    let (sync_dir, _db_path) = crate::api::sync::sync_paths(ctx)?;

    // Decide + reconstruct with the DB borrow scoped OUT before any `.await`.
    let dir = {
        let db = db(ctx)?;
        let conn = db.conn();
        match authorize_and_reconstruct_serve(&conn, &sync_dir, &from, &project_id, &package_id)? {
            Some(dir) => dir,
            None => return Ok(()), // silently refused (already warned)
        }
    };

    // Serve: enqueue an explicit-target send of the reconstructed dir to `from`
    // through the dedicated collab sender map. The manifest carries the project
    // stamp, so the engine advertises via `announce_project` and skips the
    // Offer/Want dedup negotiation automatically (T1 behavior); terminal cleanup
    // routes through the `CollabCleanupSink`.
    let (engine, _origin) = ensure_collab_sender_engine(ctx, sender, from, emitter).await?;
    engine
        .enqueue_package(&dir)
        .await
        .with_context(|| format!("enqueue collab serve dir {}", dir.display()))?;
    tracing::info!(
        project_id = %project_id,
        package_id = %package_id,
        to = %node_id_hex(&from),
        "collab project serve enqueued"
    );
    Ok(())
}

/// Ensure the collab sender engine for `dest` is running and return its handle +
/// this device's origin id. The collab counterpart of
/// [`crate::api::sync::ensure_sender_engine`]: same transport-build shape (shared
/// device identity, resolved relay map, `dest` dial hint) but a DEDICATED
/// `<sync_dir>/blobs_collab` blob store — a second `FsStore` over the
/// personal-sync `blobs_out` would risk the redb lock and the sender's startup
/// tag-sweep (audit m7). Engines spawn via
/// [`SyncEngine::spawn_with_sink_and_emitter`] with the [`CollabCleanupSink`] so a
/// reconstructed serve dir is cleaned on terminal while a retained publication
/// survives confirm (Д4). Idempotent per destination (the runtime mutex is held
/// across the whole build so two concurrent requests can't spawn two engines).
pub async fn ensure_collab_sender_engine(
    ctx: &ServiceContext,
    sender: &SyncSenderRuntime,
    dest: NodeId,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Result<(Arc<SyncEngineHandle>, String), ApiError> {
    let mut guard = sender.lock_inner().await;
    if let Some(started) = guard.get(&dest) {
        return Ok((Arc::clone(&started.engine), started.origin_device.clone()));
    }

    let peer = dest;
    let (relay_mode, relay_urls) = crate::api::sync::resolve_relay_mode(ctx).await?;
    let (sync_dir, db_path) = crate::api::sync::sync_paths(ctx)?;

    std::fs::create_dir_all(&sync_dir)
        .map_err(|e| ApiError::Internal(format!("create sync dir {}: {e}", sync_dir.display())))?;

    // The ONE device identity — the same key file the account layer, receiver, and
    // personal-sync sender bind. Never a second identity.
    let secret = crate::account::keys::DeviceKey::load_or_create(
        &crate::account::keys::device_key_path(&sync_dir),
    )
    .map_err(|e| ApiError::Internal(format!("device key: {e:#}")))?
    .secret_bytes();

    // DEDICATED `blobs_collab` store (audit m7): distinct from the receiver's
    // `blobs` and the personal-sync sender's `blobs_out`, so this engine's startup
    // tag-sweep can never race either.
    let transport = crate::sharing::iroh::IrohTransport::new(
        secret,
        relay_mode,
        crate::sharing::iroh::BlobStore::Fs(sync_dir.join("blobs_collab")),
        // Serve-only: no inbound offers to answer (project serves full-send).
        None,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("build iroh transport for collab sender: {e:#}")))?;
    let origin_device = node_id_hex(&transport.node_id());

    // The destination is an account/membership-resolved bare node id. Attach our
    // own resolved relay URL(s) as its dial hint before the first announce (same
    // reasoning as the personal-sync sender).
    let peer_addr = pairing::peer_addr_with_relays(peer, &relay_urls)
        .map_err(|e| ApiError::Internal(format!("construct peer address: {e:#}")))?;
    transport.add_peer(peer_addr);

    let transport: Arc<dyn SharingTransport> = Arc::new(transport);

    let store = Arc::new(
        CatalogSyncStore::open(&db_path)
            .map_err(|e| ApiError::Internal(format!("open catalog sync store: {e:#}")))?,
    );
    let engine = Arc::new(SyncEngine::spawn_with_sink_and_emitter(
        store as Arc<dyn SyncStore>,
        transport,
        peer,
        Arc::new(CollabCleanupSink),
        emitter,
    ));

    tracing::info!(peer = %node_id_hex(&peer), origin = %origin_device, "collab sender engine started");
    guard.insert(
        dest,
        StartedSender {
            engine: Arc::clone(&engine),
            origin_device: origin_device.clone(),
            peer,
        },
    );
    Ok((engine, origin_device))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::collab::{upsert_project, CollabProjectRow};
    use crate::db::collab_exchange::{
        contributions_for_package, insert_contribution, upsert_package, PackageRow,
    };
    use crate::package::{ManifestRecord, PayloadKind, ProjectStamp, MANIFEST_VERSION};
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;
    use rusqlite::Connection;
    use std::time::Duration;

    const NODE_COORD: NodeId = [0xA1; 32]; // coordinator + send_receive
    const NODE_SEND_ONLY: NodeId = [0xB2; 32]; // send-only contributor
    const NODE_SR: NodeId = [0xC3; 32]; // send_receive, non-coordinator
    const STRANGER: NodeId = [0xD4; 32]; // not a member

    /// In-memory catalog with the full schema + FK enforcement.
    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn
    }

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
                pending_announcements: 0,
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
                fetched_at: String::new(),
            },
        )
        .unwrap();
    }

    /// Default remote package row (tests mutate the fields they care about).
    fn base_package(hub: &str, project: &str, publisher: &str) -> PackageRow {
        PackageRow {
            package_id: hub.to_string(),
            project_id: project.to_string(),
            announcement_id: format!("ann-{hub}"),
            publisher_display: publisher.to_string(),
            own: false,
            root_hash: "rh".to_string(),
            byte_size: 0,
            frame_count: 1,
            manifest_xxh3: None,
            aggregate_stats: "{}".to_string(),
            supersedes: "[]".to_string(),
            state: "published".to_string(),
            reject_reason: None,
            superseded: false,
            origin: "remote".to_string(),
            local_dir: None,
            manifest_ndjson: None,
            local_status: "none".to_string(),
            holder_count: 0,
            online_count: 0,
            created_at: "2026-07-13 00:00:00".to_string(),
            decided_at: None,
            fetched_at: String::new(),
        }
    }

    /// Build the NDJSON manifest bytes + anchor for a one-frame stamped package.
    fn one_frame_manifest(
        rel_path: &str,
        payload: &[u8],
        project_id: &str,
        hub_package_id: &str,
    ) -> (Vec<u8>, String, ManifestRecord) {
        let rec = ManifestRecord {
            v: MANIFEST_VERSION,
            frame_uuid: "u-1".to_string(),
            origin_catalog_uuid: "cat".to_string(),
            origin_device: "de".repeat(32),
            payload_kind: PayloadKind::CalibratedLight,
            rel_path: rel_path.to_string(),
            byte_size: payload.len() as u64,
            xxh3: hash_bytes(payload),
            frame_meta: serde_json::json!({ "object": "M42" }),
            analysis: None,
            app_version: "test".to_string(),
            project: Some(ProjectStamp {
                project_id: project_id.to_string(),
                package_id: hub_package_id.to_string(),
                thresholds_version: None,
                cal_engine_version: None,
            }),
        };
        let mut buf = String::new();
        buf.push_str(&serde_json::to_string(&rec).unwrap());
        buf.push('\n');
        let bytes = buf.into_bytes();
        let anchor = hash_bytes(&bytes);
        (bytes, anchor, rec)
    }

    /// Seed a received-complete package: writes the landed payload on disk at
    /// `<landing>/<hub>/L_0001.fits`, upserts the package row (origin=received,
    /// retained manifest, given `state`, local_status=complete) + one contribution
    /// row pointing at the landed file. Returns the retained manifest bytes.
    fn seed_received_package(
        conn: &Connection,
        landing: &Path,
        project_id: &str,
        hub: &str,
        publisher: &str,
        state: &str,
        payload: &[u8],
    ) -> Vec<u8> {
        let rel_path = "L_0001.fits";
        let (manifest_bytes, anchor, rec) =
            one_frame_manifest(rel_path, payload, project_id, hub);
        let landed = landing.join(hub).join(rel_path);
        std::fs::create_dir_all(landed.parent().unwrap()).unwrap();
        std::fs::write(&landed, payload).unwrap();

        let mut row = base_package(hub, project_id, publisher);
        row.origin = "received".to_string();
        row.state = state.to_string();
        row.local_status = "complete".to_string();
        row.manifest_xxh3 = Some(anchor);
        row.manifest_ndjson = Some(manifest_bytes.clone());
        row.byte_size = payload.len() as i64;
        upsert_package(conn, &row).unwrap();

        insert_contribution(
            conn,
            &ContributionRow {
                id: 0,
                project_id: project_id.to_string(),
                package_id: hub.to_string(),
                frame_uuid: rec.frame_uuid.clone(),
                publisher_display: publisher.to_string(),
                rel_path: rel_path.to_string(),
                landed_path: landed.to_string_lossy().to_string(),
                byte_size: payload.len() as i64,
                xxh3: rec.xxh3.clone(),
                frame_meta: "{}".to_string(),
                analysis: None,
                superseded: false,
                created_at: String::new(),
            },
        )
        .unwrap();
        manifest_bytes
    }

    // ── Step 1: reconstruct_serve_dir ────────────────────────────────────────

    /// A received package materializes a serve dir with a byte-identical manifest
    /// and payloads hard-linked from the landed files.
    #[cfg(unix)]
    #[test]
    fn reconstruct_received_is_byte_identical_and_hardlinked() {
        use std::os::unix::fs::MetadataExt;
        let tmp = tempfile::tempdir().unwrap();
        let conn = test_conn();
        let landing = tmp.path().join("land");
        let payload = b"the-frame-payload-bytes";
        let manifest_bytes =
            seed_received_package(&conn, &landing, "p-1", "hub-1", "Alice", "published", payload);

        let sync_dir = tmp.path().join("sync");
        let dir = reconstruct_serve_dir(&conn, &sync_dir, "hub-1").unwrap();
        assert_eq!(dir, sync_dir.join("collab_serve").join("hub-1"));

        // Manifest byte-exact.
        let got = std::fs::read(dir.join(crate::package::MANIFEST_FILENAME)).unwrap();
        assert_eq!(got, manifest_bytes, "manifest.ndjson byte-identical to the retained bytes");

        // Payload present, correct content, and HARD-LINKED to the landed file.
        let served = dir.join("L_0001.fits");
        assert_eq!(std::fs::read(&served).unwrap(), payload);
        let landed = landing.join("hub-1").join("L_0001.fits");
        let m_served = std::fs::metadata(&served).unwrap();
        let m_landed = std::fs::metadata(&landed).unwrap();
        assert_eq!(m_served.ino(), m_landed.ino(), "serve payload is a hard link (same inode)");
        assert_eq!(m_served.dev(), m_landed.dev());
        assert!(m_landed.nlink() >= 2, "landed file now has >=2 links");

        // The reconstructed dir validates against its own manifest end to end.
        crate::package::validate_package(&dir).unwrap();
    }

    /// A second reconstruct call is idempotent: same dir, payload untouched.
    #[cfg(unix)]
    #[test]
    fn reconstruct_second_call_is_idempotent() {
        use std::os::unix::fs::MetadataExt;
        let tmp = tempfile::tempdir().unwrap();
        let conn = test_conn();
        let landing = tmp.path().join("land");
        let payload = b"payload-xyz";
        seed_received_package(&conn, &landing, "p-1", "hub-1", "Alice", "published", payload);
        let sync_dir = tmp.path().join("sync");

        let dir1 = reconstruct_serve_dir(&conn, &sync_dir, "hub-1").unwrap();
        let ino1 = std::fs::metadata(dir1.join("L_0001.fits")).unwrap().ino();
        let dir2 = reconstruct_serve_dir(&conn, &sync_dir, "hub-1").unwrap();
        assert_eq!(dir1, dir2);
        let ino2 = std::fs::metadata(dir2.join("L_0001.fits")).unwrap().ino();
        assert_eq!(ino1, ino2, "idempotent — payload untouched on the second call");
        crate::package::validate_package(&dir2).unwrap();
    }

    /// origin='mine' returns the retained local_dir as-is and never materializes a
    /// collab_serve dir.
    #[test]
    fn reconstruct_mine_returns_local_dir_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = test_conn();
        let pub_dir = tmp.path().join("collab_pub").join("hub-mine");
        std::fs::create_dir_all(&pub_dir).unwrap();
        std::fs::write(pub_dir.join(crate::package::MANIFEST_FILENAME), b"retained-manifest\n").unwrap();

        let mut row = base_package("hub-mine", "p-1", "Me");
        row.own = true;
        row.origin = "mine".to_string();
        row.local_dir = Some(pub_dir.to_string_lossy().to_string());
        upsert_package(&conn, &row).unwrap();

        let sync_dir = tmp.path().join("sync");
        let dir = reconstruct_serve_dir(&conn, &sync_dir, "hub-mine").unwrap();
        assert_eq!(dir, pub_dir, "origin=mine returns the retained local_dir");
        assert!(!sync_dir.join("collab_serve").exists(), "mine never materializes a serve dir");
        assert_eq!(
            std::fs::read(pub_dir.join(crate::package::MANIFEST_FILENAME)).unwrap(),
            b"retained-manifest\n",
            "local_dir left untouched"
        );
    }

    // ── Step 1: CollabCleanupSink ────────────────────────────────────────────

    /// The sink cleans a reconstructed `collab_serve` dir on terminal but refuses
    /// a retained `collab_pub` publication (Д4). Idempotent on an already-gone dir.
    #[test]
    fn cleanup_sink_deletes_serve_but_never_pub() {
        let tmp = tempfile::tempdir().unwrap();
        let serve = tmp.path().join("collab_serve").join("pkg-x");
        let pubd = tmp.path().join("collab_pub").join("pkg-x");
        for d in [&serve, &pubd] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join("manifest.ndjson"), b"x").unwrap();
        }
        let sink = CollabCleanupSink;
        sink.on_terminal(&serve);
        sink.on_terminal(&pubd);
        assert!(!serve.exists(), "a reconstructed collab_serve dir is cleaned on terminal");
        assert!(pubd.exists(), "a retained collab_pub publication survives (Д4)");
        // Idempotent — a second terminal on the already-gone serve dir is a no-op.
        sink.on_terminal(&serve);
    }

    // ── Step 3: authorize_and_reconstruct_serve decision matrix ──────────────

    /// A published package is served to any `send_receive` member or the
    /// coordinator; a send-only contributor and a stranger are refused.
    #[test]
    fn authorize_published_allows_sr_and_coordinator_refuses_send_only() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = test_conn();
        seed_project(&conn, "p-1", &members_json());
        let landing = tmp.path().join("land");
        seed_received_package(&conn, &landing, "p-1", "hub-1", "Alice", "published", b"payload");
        let sync_dir = tmp.path().join("sync");

        let auth = |node: &NodeId| {
            authorize_and_reconstruct_serve(&conn, &sync_dir, node, "p-1", "hub-1").unwrap()
        };
        assert!(auth(&NODE_COORD).is_some(), "coordinator is served");
        assert!(auth(&NODE_SR).is_some(), "send_receive member is served a published package");
        assert!(auth(&NODE_SEND_ONLY).is_none(), "send-only contributor is refused");
        assert!(auth(&STRANGER).is_none(), "a non-member is refused");
    }

    /// A still-pending package is served ONLY to the coordinator.
    #[test]
    fn authorize_pending_is_coordinator_only() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = test_conn();
        seed_project(&conn, "p-1", &members_json());
        let landing = tmp.path().join("land");
        seed_received_package(&conn, &landing, "p-1", "hub-p", "Alice", "pending", b"payload");
        let sync_dir = tmp.path().join("sync");

        let auth = |node: &NodeId| {
            authorize_and_reconstruct_serve(&conn, &sync_dir, node, "p-1", "hub-p").unwrap()
        };
        assert!(auth(&NODE_COORD).is_some(), "pending → coordinator is served");
        assert!(auth(&NODE_SR).is_none(), "pending → send_receive non-coordinator refused");
        assert!(auth(&NODE_SEND_ONLY).is_none(), "pending → send-only refused");
    }

    /// Unknown / incomplete / project-mismatch requests are refused as Ok(None).
    #[test]
    fn authorize_refuses_unknown_incomplete_and_project_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = test_conn();
        seed_project(&conn, "p-1", &members_json());
        let sync_dir = tmp.path().join("sync");
        let landing = tmp.path().join("land");

        // Unknown package row.
        assert!(authorize_and_reconstruct_serve(&conn, &sync_dir, &NODE_COORD, "p-1", "nope")
            .unwrap()
            .is_none());

        // Received but NOT complete.
        seed_received_package(&conn, &landing, "p-1", "hub-inc", "Alice", "published", b"payload");
        crate::db::collab_exchange::set_local_status(&conn, "hub-inc", "downloading").unwrap();
        assert!(authorize_and_reconstruct_serve(&conn, &sync_dir, &NODE_COORD, "p-1", "hub-inc")
            .unwrap()
            .is_none());

        // Complete, but the request names a DIFFERENT project than the package's.
        seed_received_package(&conn, &landing, "p-1", "hub-ok", "Alice", "published", b"payload2");
        assert!(authorize_and_reconstruct_serve(&conn, &sync_dir, &NODE_COORD, "p-OTHER", "hub-ok")
            .unwrap()
            .is_none());
        // Sanity: the same package with the correct project id is served.
        assert!(authorize_and_reconstruct_serve(&conn, &sync_dir, &NODE_COORD, "p-1", "hub-ok")
            .unwrap()
            .is_some());
    }

    /// A refused request through `handle_project_request` returns Ok and never
    /// starts a collab sender engine (the refusal short-circuits before any
    /// transport build) — the "send-only node silently refused" case, hermetic.
    #[tokio::test]
    async fn refused_request_starts_no_collab_engine() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            seed_project(&conn, "p-1", &members_json());
            seed_received_package(&conn, &tmp.path().join("land"), "p-1", "hub-1", "Alice", "published", b"payload");
        }
        let sender = SyncSenderRuntime::new();
        // A send-only contributor's request is silently refused.
        handle_project_request(&ctx, &sender, NODE_SEND_ONLY, "p-1".into(), "hub-1".into(), None)
            .await
            .unwrap();
        assert!(!sender.is_started().await, "a refused request never starts a collab engine");
        assert!(sender.started_peers().await.is_empty());
    }

    // ── Step 3: loopback serve e2e ───────────────────────────────────────────

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

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// End-to-end (loopback): A holds a received-complete package, reconstructs the
    /// serve dir, and enqueues it to B through a `spawn_with_sink_and_emitter`
    /// collab engine. The stamped manifest routes the engine through
    /// `announce_project` (no Offer/Want); B (project gate authorizes A, hub row
    /// pre-seeded) fetches, ingests the contribution (never files/frames), and
    /// acks — A's outbound row confirms and its reconstructed serve dir is cleaned
    /// while the landed source survives.
    #[cfg(unix)]
    #[tokio::test]
    async fn served_package_lands_on_b_and_confirms_on_a() {
        use crate::sharing::loopback::{LoopbackNetwork, LoopbackTransport};
        use crate::sync::{
            allow_all_peers, OutboundState, ProjectAnnounceGate, ProjectReceiveHooks,
            StandaloneSyncStore, SyncReceiver,
        };

        const PROJECT_ID: &str = "proj-serve";
        const HUB: &str = "hub-serve-1";
        const PUBLISHER: &str = "Alice Serve";

        let tmp = tempfile::tempdir().unwrap();
        let payload = b"served-frame-bytes-0001";

        // A: seed a received-complete package + reconstruct the serve dir.
        let a_db = crate::db::Database::new(tmp.path().join("a_catalog.db")).unwrap();
        let a_landing = tmp.path().join("a_land");
        let manifest_bytes = {
            let c = a_db.conn();
            seed_received_package(&c, &a_landing, PROJECT_ID, HUB, PUBLISHER, "published", payload)
        };
        let anchor = hash_bytes(&manifest_bytes);
        let a_sync_dir = tmp.path().join("a_sync");
        let serve_dir = {
            let c = a_db.conn();
            reconstruct_serve_dir(&c, &a_sync_dir, HUB).unwrap()
        };

        // B: seed the project (slug) + the hub-anchored package row + collab root.
        let b_catalog = tmp.path().join("b_catalog.db");
        let b_db = crate::db::Database::new(b_catalog.clone()).unwrap();
        let b_landing = tmp.path().join("b_land");
        {
            let c = b_db.conn();
            seed_project(&c, PROJECT_ID, "[]");
            let mut row = base_package(HUB, PROJECT_ID, PUBLISHER);
            row.manifest_xxh3 = Some(anchor.clone());
            upsert_package(&c, &row).unwrap();
            c.execute(
                "INSERT INTO scan_roots (path, kind) VALUES (?1, 'collaboration')",
                [b_landing.to_string_lossy().to_string()],
            )
            .unwrap();
        }

        // Loopback: A's collab engine ↔ B's receiver.
        let net = LoopbackNetwork::new();
        let a_ep = net.endpoint();
        let a_node = a_ep.node_id();
        let b_ep: Arc<LoopbackTransport> = Arc::new(net.endpoint());
        let b_node = b_ep.node_id();

        // B receiver — project gate authorizes A for this project.
        let b_store = Arc::new(CatalogSyncStore::open(&b_catalog).unwrap());
        let gate: ProjectAnnounceGate =
            Arc::new(move |from: &NodeId, pid: &str| *from == a_node && pid == PROJECT_ID);
        let b_incoming = tmp.path().join("b_incoming_unused");
        let (_info, _b_handle) = SyncReceiver::spawn(
            Arc::clone(&b_store),
            tmp.path().join("b_stage"),
            Arc::new(move || b_incoming.clone()),
            allow_all_peers(),
            ProjectReceiveHooks { gate: Some(gate), ..Default::default() },
            Arc::clone(&b_ep) as Arc<dyn SharingTransport>,
            Arc::new(crate::events::NullEmitter),
        )
        .await
        .unwrap();

        // A collab engine targeting B, with the CollabCleanupSink.
        let a_store = Arc::new(StandaloneSyncStore::open(tmp.path().join("a_sync.db")).unwrap());
        let a_engine = SyncEngine::spawn_with_sink_and_emitter(
            Arc::clone(&a_store) as Arc<dyn SyncStore>,
            Arc::new(a_ep) as Arc<dyn SharingTransport>,
            b_node,
            Arc::new(CollabCleanupSink),
            None,
        );

        let id = a_engine.enqueue_package(&serve_dir).await.unwrap();
        wait_until(
            || {
                a_store.get_outbound(id).ok().flatten().map(|r| r.state)
                    == Some(OutboundState::Confirmed)
            },
            Duration::from_secs(5),
        )
        .await;

        // B landed exactly one contribution (never into files/frames) with the
        // served bytes.
        let rows = {
            let c = b_db.conn();
            assert_eq!(count(&c, "SELECT COUNT(*) FROM files"), 0, "contributions never enter files");
            assert_eq!(count(&c, "SELECT COUNT(*) FROM frames"), 0, "contributions never enter frames");
            contributions_for_package(&c, HUB).unwrap()
        };
        assert_eq!(rows.len(), 1, "B landed the served frame as a contribution");
        assert_eq!(
            std::fs::read(&rows[0].landed_path).unwrap(),
            payload,
            "B's landed bytes match the served payload"
        );

        // The reconstructed serve dir is cleaned on terminal, but A's original
        // landed source survives (hard link — one link dropped).
        wait_until(|| !serve_dir.exists(), Duration::from_secs(5)).await;
        assert!(
            a_landing.join(HUB).join("L_0001.fits").exists(),
            "A's landed source is retained after the serve dir is cleaned"
        );

        a_engine.shutdown().await;
    }
}

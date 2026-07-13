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

use crate::account::keys::{device_key_path, DeviceKey};
use crate::api::{db, ApiError};
use crate::collab::hub_client::{AnnouncementWire, CollabClient, HolderWire};
use crate::db::collab_exchange::{
    get_package, mark_superseded, set_local_status, upsert_package, ContributionRow, PackageRow,
};
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

// ── Announcements poll + download orchestration (slice 4, task 8) ────────────
//
// The receive/coordinate side of the exchange: `refresh_project_packages` polls
// the hub's announcement list into `project_packages` (upsert + state diffs the
// frontend turns into `notify()`), and `download_project_package` runs the Д6
// explicit sequential-holder pull. Ungated, like the rest of this module.

/// A holder counts as ONLINE when the hub last saw it within this many seconds.
const HOLDER_ONLINE_WINDOW_SECS: i64 = 300;

/// Download loop cadence: re-read the row this often, up to the timeout, waiting
/// for a requested serve to ingest (the Task-5 receiver arm flips `local_status`).
const DOWNLOAD_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
const DOWNLOAD_POLL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// One state change a poll observed, which the frontend (Task 11) turns into a
/// `notify()` call. [`refresh_project_packages`] NEVER notifies itself.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PackageStateChange {
    pub project_id: String,
    pub package_id: String,
    /// `newPackage` | `approved` | `rejected` | `downloadComplete` | `downloadFailed`.
    pub kind: String,
    pub detail: Option<String>,
}

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
        E::Network(m) => ApiError::Internal(format!("Hub request failed: {m}")),
    }
}

/// Is this holder online — did the hub see it within [`HOLDER_ONLINE_WINDOW_SECS`]
/// of `now`? An absent or unparseable `last_seen_at` ⇒ offline.
fn holder_online(h: &HolderWire, now: chrono::DateTime<chrono::Utc>) -> bool {
    h.last_seen_at
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| (now - t.with_timezone(&chrono::Utc)).num_seconds().abs() <= HOLDER_ONLINE_WINDOW_SECS)
        .unwrap_or(false)
}

/// Diff each announcement against the existing row, then upsert it. Returns the
/// `PackageStateChange`s the poll observed. Pure (no network, no notify) so it is
/// exercised hermetically and reused by [`download_project_package`]'s fresh poll.
///
/// Diff rules (brief): an unknown row that is `published` and not mine ⇒
/// `newPackage`; a known OWN row moving `pending → published` ⇒ `approved`; any
/// known row moving to `rejected` ⇒ `rejected` (carrying the hub reason). The
/// upsert's forward-only origin + preserved local progress (T3) mean a poll can't
/// downgrade a received/owned row or clobber its manifest / `local_status`.
///
/// T3 hazard: `superseded` is a hub-mirrored column overwritten on EVERY upsert,
/// so this re-marks the complete union of the hub-listed `supersedes` arrays at
/// the end of the cycle — otherwise an own-published supersede flag would clear.
fn apply_announcements(
    conn: &rusqlite::Connection,
    project_id: &str,
    anns: &[AnnouncementWire],
) -> Result<Vec<PackageStateChange>, ApiError> {
    let now = chrono::Utc::now();
    let mut changes = Vec::new();
    let mut supersedes_union: Vec<String> = Vec::new();

    for ann in anns {
        let existing = get_package(conn, &ann.package_id)?;

        // Diff BEFORE the upsert overwrites the hub-mirrored state.
        match &existing {
            None => {
                if ann.state == "published" && !ann.own {
                    changes.push(PackageStateChange {
                        project_id: project_id.to_string(),
                        package_id: ann.package_id.clone(),
                        kind: "newPackage".to_string(),
                        detail: None,
                    });
                }
            }
            Some(row) => {
                if ann.state == "rejected" && row.state != "rejected" {
                    changes.push(PackageStateChange {
                        project_id: project_id.to_string(),
                        package_id: ann.package_id.clone(),
                        kind: "rejected".to_string(),
                        detail: ann.reject_reason.clone(),
                    });
                } else if ann.own && row.state == "pending" && ann.state == "published" {
                    changes.push(PackageStateChange {
                        project_id: project_id.to_string(),
                        package_id: ann.package_id.clone(),
                        kind: "approved".to_string(),
                        detail: None,
                    });
                }
            }
        }

        let manifest_xxh3 = ann
            .aggregate_stats
            .get("manifestXxh3")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let online_count = ann.holders.iter().filter(|h| holder_online(h, now)).count() as i64;

        upsert_package(
            conn,
            &PackageRow {
                package_id: ann.package_id.clone(),
                project_id: project_id.to_string(),
                announcement_id: ann.id.clone(),
                publisher_display: ann.publisher_display_name.clone(),
                own: ann.own,
                root_hash: ann.root_hash.clone(),
                byte_size: ann.byte_size,
                frame_count: ann.frame_count as i64,
                manifest_xxh3,
                aggregate_stats: ann.aggregate_stats.to_string(),
                supersedes: serde_json::to_string(&ann.supersedes).unwrap_or_else(|_| "[]".into()),
                state: ann.state.clone(),
                reject_reason: ann.reject_reason.clone(),
                // Re-derived comprehensively below — never trust this per-row.
                superseded: false,
                origin: if ann.own { "mine" } else { "remote" }.to_string(),
                // Local-only columns: the upsert preserves the existing values, so
                // a poll never clobbers a retained dir / manifest / fetch progress.
                local_dir: None,
                manifest_ndjson: None,
                local_status: "none".to_string(),
                holder_count: ann.holders.len() as i64,
                online_count,
                created_at: ann.created_at.clone(),
                decided_at: ann.decided_at.clone(),
                fetched_at: String::new(),
            },
        )?;
        supersedes_union.extend(ann.supersedes.iter().cloned());
    }

    // Comprehensive re-mark (T3 hazard) — mark_superseded no-ops on an empty slice.
    mark_superseded(conn, &supersedes_union)?;
    Ok(changes)
}

/// Fetch one project's announcements from the hub and apply them. Returns the raw
/// wire announcements (holders included — the download loop needs them) plus the
/// state diffs. Signed out ⇒ `Ok((vec![], vec![]))` so the poll cadence degrades
/// quietly instead of erroring.
async fn poll_project_announcements(
    ctx: &ServiceContext,
    project_id: &str,
) -> Result<(Vec<AnnouncementWire>, Vec<PackageStateChange>), ApiError> {
    let Some((hub_url, token)) = crate::api::account::hub_credentials(ctx)? else {
        return Ok((Vec::new(), Vec::new()));
    };
    let client = CollabClient::new(&hub_url).map_err(client_err)?;
    let anns = client
        .list_announcements(&token, project_id)
        .await
        .map_err(client_err)?;
    let changes = {
        let db = db(ctx)?;
        let conn = db.conn();
        apply_announcements(&conn, project_id, &anns)?
    };
    Ok((anns, changes))
}

/// Poll one project's announcements into `project_packages`. Returns the diffs the
/// frontend turns into `notify()` calls. NEVER notifies itself.
pub async fn refresh_project_packages(
    ctx: &ServiceContext,
    project_id: &str,
) -> Result<Vec<PackageStateChange>, ApiError> {
    let (_anns, changes) = poll_project_announcements(ctx, project_id).await?;
    Ok(changes)
}

/// All cached projects (the poll-cadence entry point). A per-project failure is
/// logged and skipped so one unreachable project never sinks the whole sweep.
pub async fn refresh_all_project_packages(
    ctx: &ServiceContext,
) -> Result<Vec<PackageStateChange>, ApiError> {
    let project_ids: Vec<String> = {
        let db = db(ctx)?;
        let conn = db.conn();
        crate::db::collab::list_projects(&conn)?
            .into_iter()
            .map(|p| p.project_id)
            .collect()
    };
    let mut all = Vec::new();
    for pid in project_ids {
        match refresh_project_packages(ctx, &pid).await {
            Ok(mut c) => all.append(&mut c),
            Err(e) => {
                tracing::warn!(project_id = %pid, error = %format!("{e}"), "refresh project packages failed; continuing")
            }
        }
    }
    Ok(all)
}

/// Report to the hub that this device now holds `package_id`'s blobs — the
/// task-8 post-ingest hook. Resolves the announcement id from the local row, then
/// `POST /announcements/{id}/have` with the device bearer. Signed out or an
/// unknown local row ⇒ a benign no-op (nothing to report against).
pub async fn report_have_after_ingest(
    ctx: &ServiceContext,
    package_id: &str,
) -> Result<(), ApiError> {
    let Some((hub_url, token)) = crate::api::account::hub_credentials(ctx)? else {
        return Ok(());
    };
    let announcement_id = {
        let db = db(ctx)?;
        let conn = db.conn();
        match get_package(&conn, package_id)? {
            Some(row) => row.announcement_id,
            None => {
                tracing::warn!(package_id, "report_have: no local package row; skipping");
                return Ok(());
            }
        }
    };
    let client = CollabClient::new(&hub_url).map_err(client_err)?;
    client
        .report_have(&token, &announcement_id)
        .await
        .map_err(client_err)?;
    tracing::info!(package_id, announcement_id = %announcement_id, "reported have to hub");
    Ok(())
}

/// Д6 explicit download of a project package: role-gate, poll the hub for the
/// package's current holders, then try each holder sequentially — attach a dial
/// hint (audit B4), `request_project`, and wait for the served package to ingest
/// (the Task-5 receiver arm flips `local_status` to `complete`). The first
/// success reports-have to the hub and returns; all holders exhausted lands
/// `failed`.
///
/// **Dial hints are mandatory on the real network (audit B4).** A bare
/// account-resolved holder node id is undialable: without a
/// [`pairing::peer_addr_with_relays`] hint attached via the inherent
/// [`IrohTransport::add_peer`](crate::sharing::iroh::IrohTransport::add_peer),
/// `request_project` falls back to a hint-less `EndpointAddr` and fails with "No
/// addressing information available". Loopback tests bypass dialing (in-process
/// mailbox routing), so [`SyncRuntime::iroh_handle`](crate::sync::SyncRuntime::iroh_handle)
/// is `None` there and the hint step is skipped.
///
/// Runs on a command-spawned task (Task 11): the terminal `local_status` +
/// `sync-finished` event carry the outcome, so returning `Ok` on an exhausted
/// attempt is normal, not an error.
pub async fn download_project_package(
    ctx: &ServiceContext,
    sync: &crate::sync::SyncRuntime,
    project_id: &str,
    package_id: &str,
) -> Result<(), ApiError> {
    let (sync_dir, _db_path) = crate::api::sync::sync_paths(ctx)?;

    // ── Role guard (fail-closed) ─────────────────────────────────────────────
    // Only a send_receive member or the coordinator may pull. Own membership is
    // resolved by matching THIS device's node id in the cached snapshot (the
    // account id is not part of the snapshot keying).
    let own_node = DeviceKey::load_or_create(&device_key_path(&sync_dir))
        .map_err(|e| ApiError::Internal(format!("device key: {e:#}")))?
        .node_id();
    {
        let db = db(ctx)?;
        let conn = db.conn();
        let allowed = match crate::collab::authz::member_for_node(&conn, project_id, &own_node) {
            Some(m) => m.coordinator || m.data_role == "send_receive",
            None => false,
        };
        if !allowed {
            return Err(ApiError::Invalid(format!(
                "this device's role in project {project_id} does not permit downloading packages"
            )));
        }
    }

    // ── Mark downloading (before any network I/O, so the UI reflects intent). ─
    {
        let db = db(ctx)?;
        let conn = db.conn();
        set_local_status(&conn, package_id, "downloading")?;
    }

    // ── Fresh poll: upsert the row AND read the package's current holders. ────
    let Some((hub_url, token)) = crate::api::account::hub_credentials(ctx)? else {
        set_download_failed(ctx, package_id);
        return Err(ApiError::SignedOut("Sign in to download a project package.".into()));
    };
    let client = CollabClient::new(&hub_url).map_err(client_err)?;
    let anns = match client.list_announcements(&token, project_id).await {
        Ok(a) => a,
        Err(e) => {
            set_download_failed(ctx, package_id);
            return Err(client_err(e));
        }
    };
    {
        let db = db(ctx)?;
        let conn = db.conn();
        apply_announcements(&conn, project_id, &anns)?;
    }

    // The freshly-polled announcement for this package (keyed by hub uuid).
    let Some(ann) = anns.into_iter().find(|a| a.package_id == package_id) else {
        tracing::warn!(project_id, package_id, "download: hub no longer lists this package");
        set_download_failed(ctx, package_id);
        return Ok(());
    };

    // Candidate holders (exclude MY own node — I can't serve myself).
    let holders: Vec<(NodeId, String)> = ann
        .holders
        .iter()
        .filter_map(|h| {
            pairing::node_id_from_pubkey_b64(&h.pubkey)
                .ok()
                .map(|n| (n, h.display_name.clone()))
        })
        .filter(|(n, _)| *n != own_node)
        .collect();

    if holders.is_empty() {
        tracing::warn!(project_id, package_id, "download: no other holder to pull from");
        set_download_failed(ctx, package_id);
        return Ok(());
    }

    // request_project rides the SAME endpoint the receiver listens on (an
    // outbound send; it never touches the receiver's single-consumer stream).
    let transport = sync.transport().await.ok_or_else(|| {
        ApiError::Internal("sync transport not started; cannot request a project package".into())
    })?;
    let iroh = sync.iroh_handle();
    // The relay map is only needed to build dial hints on the real-network path.
    let relay_urls = if iroh.is_some() {
        crate::api::sync::resolve_relay_mode(ctx).await?.1
    } else {
        Vec::new()
    };

    for (holder_node, holder_name) in &holders {
        // Audit B4: attach OUR resolved relay URL(s) as this holder's dial hint
        // before the request. A loopback runtime (no iroh handle) routes
        // in-process and needs no hint.
        if let Some(handle) = &iroh {
            match pairing::peer_addr_with_relays(*holder_node, &relay_urls) {
                Ok(addr) => handle.add_peer(addr),
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), holder = %holder_name, "download: dial hint build failed; skipping holder");
                    continue;
                }
            }
        }

        if let Err(e) = transport
            .request_project(*holder_node, project_id, package_id)
            .await
        {
            tracing::warn!(error = %format!("{e:#}"), holder = %holder_name, "download: request_project failed; next holder");
            continue;
        }
        tracing::info!(project_id, package_id, holder = %holder_name, "download: requested serve; awaiting ingest");

        // Wait for the receiver to flip local_status to complete (Task 5 ingest).
        if wait_for_local_complete(ctx, package_id).await {
            // Report-have (device bearer) so the hub adds us to the swarm.
            if let Err(e) = client.report_have(&token, &ann.id).await {
                tracing::warn!(announcement_id = %ann.id, error = %format!("{e}"), "download: report_have failed after ingest");
            }
            tracing::info!(project_id, package_id, holder = %holder_name, "download complete");
            return Ok(());
        }
        tracing::warn!(project_id, package_id, holder = %holder_name, "download: holder did not deliver in time; next holder");
    }

    // Every holder exhausted.
    set_download_failed(ctx, package_id);
    tracing::warn!(project_id, package_id, holders = holders.len(), "download failed: no holder delivered");
    Ok(())
}

/// Best-effort `set_local_status("failed")` — logged, never masks the caller's
/// own error/return.
fn set_download_failed(ctx: &ServiceContext, package_id: &str) {
    if let Ok(db) = db(ctx) {
        let conn = db.conn();
        if let Err(e) = set_local_status(&conn, package_id, "failed") {
            tracing::warn!(package_id, error = %format!("{e}"), "download: set failed status errored");
        }
    }
}

/// Poll `project_packages.local_status` every [`DOWNLOAD_POLL_INTERVAL`] up to
/// [`DOWNLOAD_POLL_TIMEOUT`], returning `true` the moment it reads `complete` and
/// `false` on `failed` or timeout.
async fn wait_for_local_complete(ctx: &ServiceContext, package_id: &str) -> bool {
    let deadline = tokio::time::Instant::now() + DOWNLOAD_POLL_TIMEOUT;
    loop {
        if let Ok(db) = db(ctx) {
            let conn = db.conn();
            match get_package(&conn, package_id) {
                Ok(Some(row)) if row.local_status == "complete" => return true,
                Ok(Some(row)) if row.local_status == "failed" => return false,
                _ => {}
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(DOWNLOAD_POLL_INTERVAL).await;
    }
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

    // ── Task 8: announcements poll + download orchestration ───────────────────

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
        use std::sync::{Mutex, OnceLock};
        #[cfg(all(feature = "render", feature = "solver"))]
        use std::sync::RwLock;

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
            active_registrations: Arc::new(Mutex::new(HashMap::new())),
            active_archives: Arc::new(Mutex::new(HashMap::new())),
            active_master_builds: Arc::new(Mutex::new(HashMap::new())),
            active_light_cal: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(all(feature = "render", feature = "solver"))]
            dso_catalog: Arc::new(RwLock::new(None)),
            #[cfg(feature = "solver")]
            star_cache: Arc::new(RwLock::new(None)),
            #[cfg(feature = "solver")]
            bright_cache: Arc::new(RwLock::new(None)),
            image_pool: Arc::new(rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap()),
            operation_queue: OperationQueue::start(),
            compute_queue: ComputeQueue::new(),
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

    /// This device's node id for `ctx`'s sync dir — the identity the download
    /// role guard resolves against the cached membership snapshot.
    fn own_node_for(ctx: &ServiceContext) -> NodeId {
        let (sync_dir, _) = crate::api::sync::sync_paths(ctx).unwrap();
        DeviceKey::load_or_create(&device_key_path(&sync_dir))
            .unwrap()
            .node_id()
    }

    /// One announcement wire row (camelCase), as `GET /announcements` returns it.
    #[allow(clippy::too_many_arguments)]
    fn ann_json(
        id: &str,
        package_id: &str,
        own: bool,
        state: &str,
        supersedes: &[&str],
        reject_reason: Option<&str>,
        holders: serde_json::Value,
    ) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "packageId": package_id,
            "publisherDisplayName": "Pub",
            "own": own,
            "rootHash": "a".repeat(64),
            "byteSize": 100,
            "frameCount": 3,
            "aggregateStats": { "manifestXxh3": "abcd1234" },
            "supersedes": supersedes,
            "state": state,
            "rejectReason": reject_reason,
            "createdAt": "2026-07-13T00:00:00Z",
            "decidedAt": null,
            "holders": holders,
        })
    }

    /// A fresh unknown PUBLISHED foreign announcement polls in as a `newPackage`
    /// diff (skipping an own one) and lands a `remote` row.
    #[tokio::test]
    async fn poll_new_published_foreign_is_new_package_skips_own() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p-1/announcements"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                ann_json("ann-f", "pkg-foreign", false, "published", &[], None, serde_json::json!([])),
                ann_json("ann-mine", "pkg-mine", true, "published", &[], None, serde_json::json!([])),
            ])))
            .mount(&server)
            .await;

        let (_tmp, ctx) = test_ctx();
        wire_hub(&ctx, &server.uri());

        let changes = refresh_project_packages(&ctx, "p-1").await.unwrap();
        assert_eq!(changes.len(), 1, "only the foreign published package is a newPackage");
        assert_eq!(changes[0].kind, "newPackage");
        assert_eq!(changes[0].package_id, "pkg-foreign");
        assert_eq!(changes[0].project_id, "p-1");

        let conn = db(&ctx).unwrap().conn();
        let foreign = get_package(&conn, "pkg-foreign").unwrap().unwrap();
        assert_eq!(foreign.origin, "remote");
        assert_eq!(foreign.state, "published");
        assert_eq!(foreign.manifest_xxh3.as_deref(), Some("abcd1234"));
        let mine = get_package(&conn, "pkg-mine").unwrap().unwrap();
        assert_eq!(mine.origin, "mine");
        assert!(mine.own);
    }

    /// An own package moving `pending → published` across two polls diffs as
    /// `approved` (and the first poll, still pending, raises nothing).
    #[tokio::test]
    async fn poll_own_pending_then_published_is_approved() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let (_tmp, ctx) = test_ctx();

        // Poll 1: own + pending → no diff (not published, and own is skipped).
        let server1 = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p-1/announcements"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                ann_json("ann-1", "pkg-own", true, "pending", &[], None, serde_json::json!([]))
            ])))
            .mount(&server1)
            .await;
        wire_hub(&ctx, &server1.uri());
        assert!(refresh_project_packages(&ctx, "p-1").await.unwrap().is_empty());
        assert_eq!(get_package(&db(&ctx).unwrap().conn(), "pkg-own").unwrap().unwrap().state, "pending");

        // Poll 2: the same package is now published → approved.
        let server2 = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p-1/announcements"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                ann_json("ann-1", "pkg-own", true, "published", &[], None, serde_json::json!([]))
            ])))
            .mount(&server2)
            .await;
        wire_hub(&ctx, &server2.uri());
        let changes = refresh_project_packages(&ctx, "p-1").await.unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, "approved");
        assert_eq!(changes[0].package_id, "pkg-own");

        // Idempotent: a third identical poll raises nothing (already published).
        let changes2 = refresh_project_packages(&ctx, "p-1").await.unwrap();
        assert!(changes2.is_empty(), "re-polling a published row does not re-approve");
    }

    /// A known package moving to `rejected` diffs as `rejected` carrying the hub
    /// reason as `detail`.
    #[tokio::test]
    async fn poll_rejected_carries_reason() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let (_tmp, ctx) = test_ctx();
        // Seed a known pending row directly.
        {
            let conn = db(&ctx).unwrap().conn();
            let mut row = base_package("pkg-r", "p-1", "Pub");
            row.own = true;
            row.origin = "mine".into();
            row.state = "pending".into();
            upsert_package(&conn, &row).unwrap();
        }

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p-1/announcements"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                ann_json("ann-r", "pkg-r", true, "rejected", &[], Some("FWHM too high"), serde_json::json!([]))
            ])))
            .mount(&server)
            .await;
        wire_hub(&ctx, &server.uri());

        let changes = refresh_project_packages(&ctx, "p-1").await.unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, "rejected");
        assert_eq!(changes[0].detail.as_deref(), Some("FWHM too high"));
        let row = get_package(&db(&ctx).unwrap().conn(), "pkg-r").unwrap().unwrap();
        assert_eq!(row.state, "rejected");
        assert_eq!(row.reject_reason.as_deref(), Some("FWHM too high"));
    }

    /// The hub-listed `supersedes` array marks the older package superseded, and —
    /// the T3 hazard — that flag SURVIVES a re-poll even though every upsert
    /// rewrites `superseded=0`. Holder/online counts are captured per poll.
    #[tokio::test]
    async fn poll_marks_supersedes_and_survives_repoll() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let (_tmp, ctx) = test_ctx();
        // A holder seen "now" is online; one seen long ago is not.
        let now = chrono::Utc::now().to_rfc3339();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p-1/announcements"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                // The new package supersedes ann-old.
                ann_json("ann-new", "pkg-new", true, "published", &["ann-old"], None, serde_json::json!([
                    { "pubkey": "cHVia2V5", "displayName": "H1", "lastSeenAt": now },
                    { "pubkey": "cHVia2V5Mg", "displayName": "H2", "lastSeenAt": "2020-01-01T00:00:00Z" },
                ])),
                // The older, still-own package whose announcement is superseded.
                ann_json("ann-old", "pkg-old", true, "published", &[], None, serde_json::json!([])),
            ])))
            .mount(&server)
            .await;
        wire_hub(&ctx, &server.uri());

        refresh_project_packages(&ctx, "p-1").await.unwrap();
        {
            let conn = db(&ctx).unwrap().conn();
            let old = get_package(&conn, "pkg-old").unwrap().unwrap();
            assert!(old.superseded, "pkg-old is superseded by ann-new's supersedes list");
            let new = get_package(&conn, "pkg-new").unwrap().unwrap();
            assert!(!new.superseded, "pkg-new is not superseded");
            assert_eq!(new.holder_count, 2, "holder_count = holders.len()");
            assert_eq!(new.online_count, 1, "only the recently-seen holder counts as online");
        }

        // Re-poll: every upsert rewrote superseded=0, but the comprehensive
        // re-mark restores it (T3 hazard).
        refresh_project_packages(&ctx, "p-1").await.unwrap();
        assert!(
            get_package(&db(&ctx).unwrap().conn(), "pkg-old").unwrap().unwrap().superseded,
            "own supersede flag survives a re-poll"
        );
    }

    /// Download role guard (fail-closed): a send-only own node is refused with
    /// `Invalid` before any network I/O.
    #[tokio::test]
    async fn download_role_guard_send_only_is_invalid() {
        let (_tmp, ctx) = test_ctx();
        let own = own_node_for(&ctx);
        {
            let conn = db(&ctx).unwrap().conn();
            let members = serde_json::json!([
                { "accountId": "acc-me", "displayName": "Me", "dataRole": "send",
                  "coordinator": false, "nodes": [B64.encode(own)] }
            ])
            .to_string();
            seed_project(&conn, "p-1", &members);
        }
        let sync = crate::sync::SyncRuntime::new();
        let err = download_project_package(&ctx, &sync, "p-1", "pkg-x").await.unwrap_err();
        assert!(matches!(err, ApiError::Invalid(_)), "send-only is fail-closed Invalid, got {err:?}");
    }

    /// Download happy path over loopback: D role-passes, polls the hub for the
    /// package's holder (A), requests the serve, A completes the Task-6 serve
    /// circuit, D's receiver ingests (flipping `local_status` to complete), the
    /// poll-loop observes it, and `report_have` hits the hub.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn download_happy_path_over_loopback() {
        use crate::sharing::loopback::LoopbackNetwork;
        use crate::sync::{
            allow_all_peers, ProjectAnnounceGate, ProjectReceiveHooks, StandaloneSyncStore,
            SyncReceiver, SyncRuntime,
        };
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        const PROJECT: &str = "proj-dl";
        const HUB: &str = "hub-dl-1";

        let (dtmp, ctx) = test_ctx();
        let own_node = own_node_for(&ctx);
        let (d_sync_dir, d_db_path) = crate::api::sync::sync_paths(&ctx).unwrap();

        // ── A: a received-complete package + its reconstructed serve dir. ──────
        let a_tmp = tempfile::tempdir().unwrap();
        let payload = b"downloaded-frame-bytes-0001";
        let a_db = crate::db::Database::new(a_tmp.path().join("a_catalog.db")).unwrap();
        let manifest_bytes = {
            let c = a_db.conn();
            seed_received_package(&c, &a_tmp.path().join("a_land"), PROJECT, HUB, "Alice", "published", payload)
        };
        // The hub-anchored manifest hash the receiver verifies the served bytes
        // against — must be the REAL anchor of A's manifest, not a placeholder.
        let anchor = hash_bytes(&manifest_bytes);
        let serve_dir = {
            let c = a_db.conn();
            reconstruct_serve_dir(&c, &a_tmp.path().join("a_sync"), HUB).unwrap()
        };

        // ── Loopback endpoints: D (receiver + request sender), A recv + A send. ─
        let net = LoopbackNetwork::new();
        let d_ep: Arc<crate::sharing::loopback::LoopbackTransport> = Arc::new(net.endpoint());
        let d_node = d_ep.node_id();
        let a_recv_ep = net.endpoint();
        let a_recv_node = a_recv_ep.node_id();
        let a_send_ep = net.endpoint();
        let a_send_node = a_send_ep.node_id();

        // A's collab engine serves back to D over a_send.
        let a_store = Arc::new(StandaloneSyncStore::open(a_tmp.path().join("a_sync.db")).unwrap());
        let a_engine = Arc::new(SyncEngine::spawn_with_sink_and_emitter(
            Arc::clone(&a_store) as Arc<dyn SyncStore>,
            Arc::new(a_send_ep) as Arc<dyn SharingTransport>,
            d_node,
            Arc::new(CollabCleanupSink),
            None,
        ));

        // A's receiver: on an inbound request, enqueue the reconstructed serve dir.
        let handler_engine = Arc::clone(&a_engine);
        let handler_dir = serve_dir.clone();
        let request_handler: crate::sync::ProjectRequestHandler =
            Arc::new(move |from: NodeId, _project: String, _package: String| {
                assert_eq!(from, d_node, "the serve request came from D");
                let e = Arc::clone(&handler_engine);
                let dir = handler_dir.clone();
                tokio::spawn(async move {
                    let _ = e.enqueue_package(&dir).await;
                });
            });
        let a_recv_store = Arc::new(CatalogSyncStore::open(a_tmp.path().join("a_recv.db")).unwrap());
        let a_incoming: crate::sync::receiver::IncomingResolver = {
            let p = a_tmp.path().join("a_incoming");
            Arc::new(move || p.clone())
        };
        let (_a_info, _a_handle) = SyncReceiver::spawn(
            a_recv_store,
            a_tmp.path().join("a_stage"),
            a_incoming,
            allow_all_peers(),
            ProjectReceiveHooks { request_handler: Some(request_handler), ..Default::default() },
            Arc::new(a_recv_ep) as Arc<dyn SharingTransport>,
            Arc::new(crate::events::NullEmitter),
        )
        .await
        .unwrap();

        // ── D: seed the catalog, spawn the receiver, hold the runtime. ─────────
        {
            let conn = db(&ctx).unwrap().conn();
            // Own node is a send_receive member → role guard passes.
            let members = serde_json::json!([
                { "accountId": "acc-me", "displayName": "Me", "dataRole": "send_receive",
                  "coordinator": true, "nodes": [B64.encode(own_node)] }
            ])
            .to_string();
            seed_project(&conn, PROJECT, &members);
            // The hub package row (from a prior poll) — origin remote, not yet held.
            let row = base_package(HUB, PROJECT, "Alice");
            upsert_package(&conn, &row).unwrap();
            // The collaboration landing root project_ingest fills.
            conn.execute(
                "INSERT INTO scan_roots (path, kind) VALUES (?1, 'collaboration')",
                [dtmp.path().join("d_land").to_string_lossy().to_string()],
            )
            .unwrap();
        }

        let d_store = Arc::new(CatalogSyncStore::open(&d_db_path).unwrap());
        // D's project gate authorizes the announce from A's SENDER node.
        let gate: ProjectAnnounceGate =
            Arc::new(move |from: &NodeId, pid: &str| *from == a_send_node && pid == PROJECT);
        let d_incoming: crate::sync::receiver::IncomingResolver = {
            let p = d_sync_dir.join("incoming");
            Arc::new(move || p.clone())
        };
        let (_d_info, d_handle) = SyncReceiver::spawn(
            d_store,
            d_sync_dir.clone(),
            d_incoming,
            allow_all_peers(),
            ProjectReceiveHooks { gate: Some(gate), ..Default::default() },
            Arc::clone(&d_ep) as Arc<dyn SharingTransport>,
            Arc::new(crate::events::NullEmitter),
        )
        .await
        .unwrap();

        let runtime = SyncRuntime::new();
        runtime
            .set_started_for_test(Arc::clone(&d_ep) as Arc<dyn SharingTransport>, d_handle, "ticket".into())
            .await;

        // ── Hub: list the package with A (recv node) as its holder + accept have. ─
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/projects/{PROJECT}/announcements")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                "id": "ann-hub",
                "packageId": HUB,
                "publisherDisplayName": "Alice",
                "own": false,
                "rootHash": "a".repeat(64),
                "byteSize": payload.len(),
                "frameCount": 1,
                // The REAL manifest anchor — the receiver verifies against it.
                "aggregateStats": { "manifestXxh3": anchor },
                "supersedes": [],
                "state": "published",
                "rejectReason": null,
                "createdAt": "2026-07-13T00:00:00Z",
                "decidedAt": null,
                "holders": [
                    { "pubkey": B64.encode(a_recv_node), "displayName": "Alice",
                      "lastSeenAt": chrono::Utc::now().to_rfc3339() }
                ],
            }])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/announcements/ann-hub/have"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        wire_hub(&ctx, &server.uri());

        // ── Drive the download. ────────────────────────────────────────────────
        download_project_package(&ctx, &runtime, PROJECT, HUB).await.unwrap();

        // D holds the package: local_status complete, one landed contribution
        // (never into files/frames), and the hub was told we now have it.
        let conn = db(&ctx).unwrap().conn();
        assert_eq!(
            get_package(&conn, HUB).unwrap().unwrap().local_status,
            "complete",
            "the download loop observed the ingest"
        );
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 0, "contributions never enter files");
        let landed = contributions_for_package(&conn, HUB).unwrap();
        assert_eq!(landed.len(), 1, "the served frame landed as a contribution");
        assert_eq!(std::fs::read(&landed[0].landed_path).unwrap(), payload);

        // report_have was asserted via `.expect(1)`; verify explicitly too.
        let haves = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.url.path() == "/api/v1/announcements/ann-hub/have")
            .count();
        assert_eq!(haves, 1, "report_have hit the hub once after ingest");

        a_engine.shutdown().await;
    }
}

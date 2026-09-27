//! The catalog-backed [`ServeOracle`] of the collab provider (spec §9.3,
//! plan P14). The provider consumer asks it on a blocking thread before every
//! get: which servable row has the requested hash as its CURRENT version,
//! where its file is and the stamp recorded when it became servable.
//!
//! Two shapes, one rule ("serve iff the row is servable for the current
//! version and the file's `size:mtime` still matches"):
//!
//! - [`DbServeOracle::catalog_only`] — installed by the app as soon as it
//!   binds the node, before any live session exists. Storage availability is
//!   the mount itself (the collab store is mounted only after a serving
//!   marker check), and a stamp mismatch has no storage engine to check it —
//!   the refusal is the whole answer until the next sweep.
//! - [`DbServeOracle::new`] — the live session's: availability follows its
//!   [`StoreGuard`], and a mismatch queues an immediate local check of that
//!   frame on the storage engine's channel.

use std::sync::Arc;

use crate::collab::serve::{ServeOracle, ServeRecord};
use crate::collab::storage::marker::StoreGuard;
use crate::collab::storage::sweep::Stamp;
use crate::db::collab_frames::LocalState;
use crate::db::Database;
use crate::services::ServiceContext;

/// The local states whose rows are served: [`LocalState::servable`]. An idle
/// row keeps its seed tags (so its blob is still in the store) but is never
/// served; neither is a wanted, missing, quarantined, declined or
/// awaiting-choice one.
///
/// An own row that is `own_staged` (its new file on disk and stamped, the
/// hub not yet confirming the new version, so `blake3` is still the old
/// one) is refused too: the old hash must never be served from new bytes.
const SERVABLE_STATES: [LocalState; 2] = [LocalState::Held, LocalState::OwnHeld];

pub struct DbServeOracle {
    db: Option<Database>,
    guard: Option<Arc<StoreGuard>>,
    checks: Option<tokio::sync::mpsc::UnboundedSender<(String, String)>>,
}

impl DbServeOracle {
    /// The live session's oracle: availability from `guard`, mismatches
    /// queued as `(project_id, frame_uuid)` on `checks`.
    pub fn new(
        ctx: Arc<ServiceContext>,
        guard: Arc<StoreGuard>,
        checks: tokio::sync::mpsc::UnboundedSender<(String, String)>,
    ) -> Self {
        Self {
            db: ctx.db.get().cloned(),
            guard: Some(guard),
            checks: Some(checks),
        }
    }

    /// The oracle without a live runtime (see the module doc).
    pub fn catalog_only(db: Database) -> Self {
        Self {
            db: Some(db),
            guard: None,
            checks: None,
        }
    }
}

impl ServeOracle for DbServeOracle {
    fn lookup(&self, blake3_hex: &str) -> Option<ServeRecord> {
        let Some(db) = self.db.as_ref() else {
            tracing::error!(
                blake3 = blake3_hex,
                "serve lookup failed: catalog not open; refusing"
            );
            return None;
        };
        let conn = db.conn();
        // Every servable row of the hash (two frames may hold identical
        // bytes, C10): the one whose file still stats as recorded is served
        // (Task 15, T10 carry — `LIMIT 1` refused a get whenever the first
        // row's stamp was stale although a sibling could serve), else the
        // first, whose mismatch queues its local check.
        let rows: Vec<(String, String, String, String)> = conn
            .prepare(
                "SELECT project_id, frame_uuid, landed_path, size_mtime_seen
                 FROM project_frames_local
                 WHERE blake3 = ?1 AND local_state IN (?2, ?3) AND own_staged = 0
                   AND landed_path IS NOT NULL AND size_mtime_seen IS NOT NULL
                 ORDER BY project_id, frame_uuid",
            )
            .and_then(|mut stmt| {
                stmt.query_map(
                    rusqlite::params![
                        blake3_hex,
                        SERVABLE_STATES[0].as_db_str(),
                        SERVABLE_STATES[1].as_db_str()
                    ],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )?
                .collect()
            })
            .unwrap_or_else(|e| {
                tracing::error!(blake3 = blake3_hex, error = %e, "serve lookup failed; refusing");
                Vec::new()
            });
        let pick = rows
            .iter()
            .position(|(_, _, path, stamp)| {
                Stamp::parse(stamp).is_some_and(|st| {
                    matches!(
                        crate::collab::storage::sweep::stat_verdict(
                            std::path::Path::new(path),
                            Some(st)
                        ),
                        crate::collab::storage::sweep::StatVerdict::Same
                    )
                })
            })
            .unwrap_or(0);
        let (project_id, frame_uuid, path, stamp) = rows.into_iter().nth(pick)?;
        let Some(stamp) = Stamp::parse(&stamp) else {
            tracing::warn!(
                project_id = %project_id,
                frame_uuid = %frame_uuid,
                value = %stamp,
                "serve lookup: unparseable size_mtime_seen; refusing"
            );
            return None;
        };
        Some(ServeRecord {
            project_id,
            frame_uuid,
            path: path.into(),
            stamp,
        })
    }

    fn serving(&self) -> bool {
        self.guard.as_ref().is_none_or(|g| g.state().serving())
    }

    fn on_mismatch(&self, rec: &ServeRecord) {
        let Some(checks) = self.checks.as_ref() else {
            tracing::debug!(
                project_id = %rec.project_id,
                frame_uuid = %rec.frame_uuid,
                "no storage engine running; the changed file waits for the next sweep"
            );
            return;
        };
        match checks.send((rec.project_id.clone(), rec.frame_uuid.clone())) {
            Ok(()) => tracing::debug!(
                project_id = %rec.project_id,
                frame_uuid = %rec.frame_uuid,
                "local check queued for the changed file"
            ),
            Err(e) => tracing::warn!(
                project_id = %rec.project_id,
                frame_uuid = %rec.frame_uuid,
                error = %e,
                "local check not queued: the storage engine stopped"
            ),
        }
    }
}

/// Install the catalog-only oracle on a freshly bound node (see the module
/// doc). A context without an open catalog installs nothing — logged.
pub(crate) fn install_catalog_oracle(
    ctx: &ServiceContext,
    node: &crate::sharing::iroh::node::SharedIrohNode,
) {
    if let Some(oracle) = catalog_oracle(ctx) {
        node.set_collab_serve_oracle(Some(oracle));
    }
}

/// The catalog-backed oracle [`install_catalog_oracle`] installs (`None`,
/// logged, when the catalog is not open).
pub(crate) fn catalog_oracle(
    ctx: &ServiceContext,
) -> Option<Arc<dyn crate::collab::serve::ServeOracle>> {
    match ctx.db.get() {
        Some(db) => Some(Arc::new(DbServeOracle::catalog_only(db.clone()))),
        None => {
            tracing::error!("collab serve oracle not installed: catalog not open");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::collab_live::test_support::{self as ts, LandedRig};
    use crate::db::collab_frames::LocalState;

    fn blake3_of(rig: &LandedRig, i: usize) -> String {
        rig.hash_of(i).to_hex().to_string()
    }

    fn catalog_oracle(rig: &LandedRig) -> DbServeOracle {
        DbServeOracle::catalog_only(crate::api::db(&rig.ctx).unwrap().clone())
    }

    fn set_state(rig: &LandedRig, i: usize, to: LocalState) {
        let (pid, uuid, _) = &rig.frames[i];
        let conn = crate::api::db(&rig.ctx).unwrap().conn();
        conn.execute(
            "UPDATE project_frames_local SET local_state = ?3 WHERE project_id = ?1 AND frame_uuid = ?2",
            rusqlite::params![pid, uuid, to.as_db_str()],
        )
        .unwrap();
    }

    /// Task 15 (T10 carry): two servable rows share a hash (C10); the one
    /// whose file still stats as recorded is served, not merely the first.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_duplicate_hash_is_served_from_the_row_whose_stamp_matches() {
        let rig = ts::landed_rig(2).await;
        let oracle = catalog_oracle(&rig);
        let hex = blake3_of(&rig, 0);
        let (pid, _, first) = rig.frames[0].clone();
        let (_, twin, _) = rig.frames[1].clone();
        let copy = rig.root.join("m31").join("other").join("twin-copy.fits");
        std::fs::copy(&first, &copy).unwrap();
        let stamp = crate::collab::storage::sweep::Stamp::of(&std::fs::metadata(&copy).unwrap());
        crate::api::db(&rig.ctx)
            .unwrap()
            .conn()
            .execute(
                "UPDATE project_frames_local SET blake3 = ?3, landed_path = ?4, size_mtime_seen = ?5
                 WHERE project_id = ?1 AND frame_uuid = ?2",
                rusqlite::params![pid, twin, hex, copy.to_string_lossy(), stamp.encode()],
            )
            .unwrap();
        assert_eq!(
            oracle.lookup(&hex).unwrap().frame_uuid,
            "f00",
            "both match: the first in order"
        );
        ts::set_mtime(&first, 10);
        let rec = oracle.lookup(&hex).expect("the twin can serve");
        assert_eq!(
            rec.frame_uuid, twin,
            "the stale first row does not hide its twin"
        );
        assert_eq!(rec.path, copy);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn lookup_returns_only_servable_rows_of_the_current_version() {
        let rig = ts::landed_rig(1).await;
        let oracle = catalog_oracle(&rig);
        let hex = blake3_of(&rig, 0);
        let (pid, uuid, path) = rig.frames[0].clone();

        let rec = oracle.lookup(&hex).expect("a held row is served");
        assert_eq!(
            (rec.project_id.as_str(), rec.frame_uuid.as_str()),
            (pid.as_str(), uuid.as_str())
        );
        assert_eq!(rec.path, path);
        assert_eq!(
            rec.stamp,
            Stamp::of(&std::fs::metadata(&path).unwrap()),
            "the recorded stamp"
        );

        // My own frame on disk is servable too.
        set_state(&rig, 0, LocalState::OwnHeld);
        assert!(oracle.lookup(&hex).is_some());

        for refused in [
            LocalState::Wanted,
            LocalState::Quarantined,
            LocalState::Idle,
            LocalState::Missing,
            LocalState::NotKept,
            LocalState::AwaitingChoice,
            LocalState::OwnMissing,
            LocalState::OwnChanged,
        ] {
            set_state(&rig, 0, refused);
            assert_eq!(oracle.lookup(&hex), None, "{refused:?} must not be served");
        }

        // A superseded hash: the row moved to v2 with other bytes.
        set_state(&rig, 0, LocalState::Held);
        {
            let conn = crate::api::db(&rig.ctx).unwrap().conn();
            conn.execute(
                "UPDATE project_frames_local SET content_version = content_version + 1,
                    blake3 = ?3 WHERE project_id = ?1 AND frame_uuid = ?2",
                rusqlite::params![pid, uuid, "00".repeat(32)],
            )
            .unwrap();
        }
        assert_eq!(
            oracle.lookup(&hex),
            None,
            "a superseded version is not served"
        );
        assert!(oracle.lookup(&"00".repeat(32)).is_some());

        // No stamp recorded ⇒ nothing to check the file against ⇒ refused.
        {
            let conn = crate::api::db(&rig.ctx).unwrap().conn();
            conn.execute(
                "UPDATE project_frames_local SET size_mtime_seen = NULL WHERE project_id = ?1 AND frame_uuid = ?2",
                rusqlite::params![pid, uuid],
            )
            .unwrap();
        }
        assert_eq!(oracle.lookup(&"00".repeat(32)), None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn availability_follows_the_guard_and_mismatches_are_queued() {
        let rig = ts::landed_rig(1).await;
        assert!(
            catalog_oracle(&rig).serving(),
            "catalog-only: the mount is the gate"
        );

        let me = crate::api::account::own_device_id(&rig.ctx).unwrap();
        let guard = Arc::new(StoreGuard::new(rig.root.clone(), me, None));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let live = DbServeOracle::new(Arc::clone(&rig.ctx), Arc::clone(&guard), tx);
        assert!(!live.serving(), "a guard that never checked is unavailable");

        let rec = live.lookup(&blake3_of(&rig, 0)).unwrap();
        live.on_mismatch(&rec);
        assert_eq!(
            rx.recv().await,
            Some((rec.project_id.clone(), rec.frame_uuid.clone()))
        );
        drop(rx);
        live.on_mismatch(&rec); // the engine stopped: logged, never panics
    }

    // ----- the collab provider's serve check, two real nodes ----------------

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_collab_provider_serves_a_held_frame_and_refuses_an_edited_one() {
        let rig = ts::landed_rig(1).await; // node B with a held replica + oracle installed
        let (pid, uuid, path) = rig.frames[0].clone();
        let fetcher = ts::bare_node().await; // node A
        ts::pair(&fetcher, &rig.node).await;
        let hash = rig.hash_of(0);
        let conn = fetcher
            .endpoint()
            .connect(
                rig.node.endpoint_addr(),
                crate::sharing::iroh::COLLAB_BLOBS_ALPN,
            )
            .await
            .unwrap();
        let store = ts::scratch_store();
        store
            .remote()
            .execute_get(conn.clone(), iroh_blobs::protocol::GetRequest::blob(hash))
            .await
            .expect("served");
        assert!(store.blobs().has(hash).await.unwrap());
        assert_eq!(
            rig.node.collab_streams_in_use(),
            0,
            "the permit is released"
        );

        // edit in place, same size: the NEXT request is refused with ERR_PERMISSION
        ts::overwrite_same_size(&path);
        let store2 = ts::scratch_store();
        let err = store2
            .remote()
            .execute_get(conn.clone(), iroh_blobs::protocol::GetRequest::blob(hash))
            .await
            .unwrap_err();
        assert_eq!(
            err.iroh_error_code(),
            Some(iroh_blobs::protocol::ERR_PERMISSION)
        );
        assert_eq!(rig.next_local_check().await, Some((pid, uuid)));
        assert!(
            !store2.blobs().has(hash).await.unwrap(),
            "not a byte of the edited file"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_idle_row_is_refused_though_its_blob_is_still_seeded() {
        let rig = ts::landed_rig(1).await;
        set_state(&rig, 0, LocalState::Idle);
        let fetcher = ts::bare_node().await;
        ts::pair(&fetcher, &rig.node).await;
        let conn = fetcher
            .endpoint()
            .connect(
                rig.node.endpoint_addr(),
                crate::sharing::iroh::COLLAB_BLOBS_ALPN,
            )
            .await
            .unwrap();
        let err = ts::scratch_store()
            .remote()
            .execute_get(conn, iroh_blobs::protocol::GetRequest::blob(rig.hash_of(0)))
            .await
            .unwrap_err();
        assert_eq!(
            err.iroh_error_code(),
            Some(iroh_blobs::protocol::ERR_PERMISSION)
        );
    }

    /// Fix round 1: a request past offset 0 would stream a hash-seq's
    /// children, which the serve check never sees — refused outright, even on
    /// a held hash.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_get_with_child_ranges_is_refused() {
        use iroh_blobs::protocol::{ChunkRanges, GetRequest};
        let rig = ts::landed_rig(1).await;
        let fetcher = ts::bare_node().await;
        ts::pair(&fetcher, &rig.node).await;
        let conn = fetcher
            .endpoint()
            .connect(
                rig.node.endpoint_addr(),
                crate::sharing::iroh::COLLAB_BLOBS_ALPN,
            )
            .await
            .unwrap();
        let request = GetRequest::builder()
            .root(ChunkRanges::all())
            .child(0, ChunkRanges::all())
            .build(rig.hash_of(0));
        let err = ts::scratch_store()
            .remote()
            .execute_get(conn, request)
            .await
            .unwrap_err();
        assert_eq!(
            err.iroh_error_code(),
            Some(iroh_blobs::protocol::ERR_PERMISSION)
        );
    }

    /// Fix round 1 (C11): an own frame staged with new bytes (its stamp
    /// recorded, the hub not yet confirming the new blake3) never serves the
    /// OLD hash from the new file; once the version is confirmed the NEW hash
    /// is served.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_staged_own_frame_is_served_only_after_its_version_is_confirmed() {
        let rig = ts::landed_rig(1).await;
        let (pid, uuid, path) = rig.frames[0].clone();
        let old = rig.hash_of(0);
        {
            let conn = crate::api::db(&rig.ctx).unwrap().conn();
            conn.execute(
                "UPDATE project_frames_local SET origin = 'own', local_state = 'own_held',
                    recipe_hash = 'r1' WHERE project_id = ?1 AND frame_uuid = ?2",
                rusqlite::params![pid, uuid],
            )
            .unwrap();
        }
        // The regenerated file replaces the landed one and is seeded as v2.
        let bytes = format!("regenerated own frame {uuid}: pixels ")
            .repeat(1024)
            .into_bytes();
        std::fs::write(&path, &bytes).unwrap();
        let new = rig
            .node
            .seed_project_frame(&pid, &uuid, 2, &path)
            .await
            .unwrap();
        let xxh3 = format!("{:016x}", xxhash_rust::xxh3::xxh3_64(&bytes));
        let stamp = Stamp::of(&std::fs::metadata(&path).unwrap()).encode();
        {
            let conn = crate::api::db(&rig.ctx).unwrap().conn();
            let n = crate::db::collab_frames::stage_own_file(
                &conn,
                &pid,
                &uuid,
                &path.to_string_lossy(),
                &xxh3,
                bytes.len() as i64,
                Some(&stamp),
            )
            .unwrap();
            assert_eq!(n, 1);
        }
        let oracle = catalog_oracle(&rig);
        assert_eq!(
            oracle.lookup(&old.to_hex()),
            None,
            "staged: old hash refused"
        );
        assert_eq!(oracle.lookup(&new.to_hex()), None, "not confirmed yet");

        let fetcher = ts::bare_node().await;
        ts::pair(&fetcher, &rig.node).await;
        let conn = fetcher
            .endpoint()
            .connect(
                rig.node.endpoint_addr(),
                crate::sharing::iroh::COLLAB_BLOBS_ALPN,
            )
            .await
            .unwrap();
        let err = ts::scratch_store()
            .remote()
            .execute_get(conn.clone(), iroh_blobs::protocol::GetRequest::blob(old))
            .await
            .unwrap_err();
        assert_eq!(
            err.iroh_error_code(),
            Some(iroh_blobs::protocol::ERR_PERMISSION)
        );

        {
            let conn = crate::api::db(&rig.ctx).unwrap().conn();
            crate::db::collab_frames::set_own_version(
                &conn,
                &pid,
                &uuid,
                2,
                &new.to_hex(),
                &xxh3,
                bytes.len() as i64,
                "r2",
                Some(&stamp),
            )
            .unwrap();
        }
        assert!(oracle.lookup(&new.to_hex()).is_some(), "confirmed: served");
        let store = ts::scratch_store();
        store
            .remote()
            .execute_get(conn, iroh_blobs::protocol::GetRequest::blob(new))
            .await
            .expect("the confirmed version is served");
        assert!(store.blobs().has(new).await.unwrap());
    }

    /// Fix round 2 (C11): the hub confirms the staged file through the
    /// manifest (same xxh3 and size as the staged bytes, the new blake3) —
    /// the row is no longer staged and the new hash is served.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_staged_own_frame_confirmed_by_the_manifest_is_served() {
        let rig = ts::landed_rig(1).await;
        let (pid, uuid, path) = rig.frames[0].clone();
        {
            let conn = crate::api::db(&rig.ctx).unwrap().conn();
            conn.execute(
                "UPDATE project_frames_local SET origin = 'own', local_state = 'own_held',
                    recipe_hash = 'r1' WHERE project_id = ?1 AND frame_uuid = ?2",
                rusqlite::params![pid, uuid],
            )
            .unwrap();
        }
        let bytes = format!("regenerated own frame {uuid}: pixels ")
            .repeat(1024)
            .into_bytes();
        std::fs::write(&path, &bytes).unwrap();
        let new = rig
            .node
            .seed_project_frame(&pid, &uuid, 2, &path)
            .await
            .unwrap();
        let xxh3 = format!("{:016x}", xxhash_rust::xxh3::xxh3_64(&bytes));
        let stamp = Stamp::of(&std::fs::metadata(&path).unwrap()).encode();
        {
            let conn = crate::api::db(&rig.ctx).unwrap().conn();
            crate::db::collab_frames::stage_own_file(
                &conn,
                &pid,
                &uuid,
                &path.to_string_lossy(),
                &xxh3,
                bytes.len() as i64,
                Some(&stamp),
            )
            .unwrap();
        }
        let oracle = catalog_oracle(&rig);
        assert_eq!(oracle.lookup(&new.to_hex()), None, "not confirmed yet");

        // The manifest delivers the confirmed version of exactly these bytes.
        rig.hub.update_frame(&pid, &uuid, |f| {
            f.content_version = 2;
            f.blake3 = new.to_hex().to_string();
            f.xxh3 = xxh3.clone();
            f.byte_size = bytes.len() as i64;
        });
        let mut view = rig.hub.frame(&pid, &uuid).unwrap();
        view.own = true;
        {
            let conn = crate::api::db(&rig.ctx).unwrap().conn();
            crate::db::collab_frames::upsert_from_manifest(&conn, &pid, &view).unwrap();
        }
        let rec = oracle
            .lookup(&new.to_hex())
            .expect("confirmed by the manifest: served");
        assert_eq!(rec.path, path);

        let fetcher = ts::bare_node().await;
        ts::pair(&fetcher, &rig.node).await;
        let conn = fetcher
            .endpoint()
            .connect(
                rig.node.endpoint_addr(),
                crate::sharing::iroh::COLLAB_BLOBS_ALPN,
            )
            .await
            .unwrap();
        let store = ts::scratch_store();
        store
            .remote()
            .execute_get(conn, iroh_blobs::protocol::GetRequest::blob(new))
            .await
            .expect("the confirmed version is served");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_upload_stream_limit_refuses_with_err_limit() {
        let rig = ts::landed_rig(1).await;
        rig.node.set_collab_upload_limit(1);
        let held = rig
            .node
            .collab_stream_gauge_for_test()
            .try_acquire()
            .unwrap(); // the one stream is busy
        assert_eq!(rig.node.collab_streams_in_use(), 1);
        let fetcher = ts::bare_node().await;
        ts::pair(&fetcher, &rig.node).await;
        let conn = fetcher
            .endpoint()
            .connect(
                rig.node.endpoint_addr(),
                crate::sharing::iroh::COLLAB_BLOBS_ALPN,
            )
            .await
            .unwrap();
        let store = ts::scratch_store();
        let err = store
            .remote()
            .execute_get(
                conn.clone(),
                iroh_blobs::protocol::GetRequest::blob(rig.hash_of(0)),
            )
            .await
            .unwrap_err();
        assert_eq!(err.iroh_error_code(), Some(iroh_blobs::protocol::ERR_LIMIT));

        // The stream frees up: the same connection is served.
        drop(held);
        store
            .remote()
            .execute_get(conn, iroh_blobs::protocol::GetRequest::blob(rig.hash_of(0)))
            .await
            .expect("served once a stream is free");
    }

    /// Fix round 1: a membership change landing between the accept-time gate
    /// checks and the registration is caught by the re-check right after
    /// registering. The gate admits its first two evaluations (the slot
    /// handler's and the provider's accept-time checks) and refuses from the
    /// third on — the post-registration re-check.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_connection_whose_member_was_dropped_while_accepting_is_closed() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let rig = ts::landed_rig(1).await;
        let fetcher = ts::bare_node().await;
        ts::pair(&fetcher, &rig.node).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        rig.node
            .set_connect_gate(Arc::new(move |_| seen.fetch_add(1, Ordering::SeqCst) < 2));
        let conn = fetcher
            .endpoint()
            .connect(
                rig.node.endpoint_addr(),
                crate::sharing::iroh::COLLAB_BLOBS_ALPN,
            )
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), conn.closed())
            .await
            .expect("closed right after registration");
        assert!(
            calls.load(Ordering::SeqCst) >= 3,
            "the gate was asked again"
        );
        assert_eq!(
            rig.node.close_collab_connections_not_admitted(),
            0,
            "nothing left registered"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_membership_change_closes_accepted_collab_connections() {
        let rig = ts::landed_rig(1).await;
        let fetcher = ts::bare_node().await;
        ts::pair(&fetcher, &rig.node).await;
        let conn = fetcher
            .endpoint()
            .connect(
                rig.node.endpoint_addr(),
                crate::sharing::iroh::COLLAB_BLOBS_ALPN,
            )
            .await
            .unwrap();
        let store = ts::scratch_store();
        store
            .remote()
            .execute_get(
                conn.clone(),
                iroh_blobs::protocol::GetRequest::blob(rig.hash_of(0)),
            )
            .await
            .unwrap();
        // Still a member: nothing is closed.
        assert_eq!(rig.node.close_collab_connections_not_admitted(), 0);
        rig.node.set_connect_gate(Arc::new(|_| false)); // the fetcher is no longer a member
        assert_eq!(rig.node.close_collab_connections_not_admitted(), 1);
        tokio::time::timeout(std::time::Duration::from_secs(5), conn.closed())
            .await
            .expect("closed by the provider");
        assert_eq!(
            rig.node.close_collab_connections_not_admitted(),
            0,
            "closed once"
        );
    }
}

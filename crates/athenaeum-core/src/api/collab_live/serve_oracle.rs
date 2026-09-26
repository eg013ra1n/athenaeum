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

use rusqlite::OptionalExtension;

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
        let row: Option<(String, String, String, String)> = conn
            .query_row(
                "SELECT project_id, frame_uuid, landed_path, size_mtime_seen
                 FROM project_frames_local
                 WHERE blake3 = ?1 AND local_state IN (?2, ?3)
                   AND landed_path IS NOT NULL AND size_mtime_seen IS NOT NULL
                 ORDER BY project_id, frame_uuid
                 LIMIT 1",
                rusqlite::params![
                    blake3_hex,
                    SERVABLE_STATES[0].as_db_str(),
                    SERVABLE_STATES[1].as_db_str()
                ],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .unwrap_or_else(|e| {
                tracing::error!(blake3 = blake3_hex, error = %e, "serve lookup failed; refusing");
                None
            });
        let (project_id, frame_uuid, path, stamp) = row?;
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
        if let Err(e) = checks.send((rec.project_id.clone(), rec.frame_uuid.clone())) {
            tracing::warn!(
                project_id = %rec.project_id,
                frame_uuid = %rec.frame_uuid,
                error = %e,
                "local check not queued: the storage engine stopped"
            );
        }
    }
}

/// Install the catalog-only oracle on a freshly bound node (see the module
/// doc). A context without an open catalog installs nothing — logged.
pub(crate) fn install_catalog_oracle(
    ctx: &ServiceContext,
    node: &crate::sharing::iroh::node::SharedIrohNode,
) {
    match ctx.db.get() {
        Some(db) => {
            node.set_collab_serve_oracle(Some(Arc::new(DbServeOracle::catalog_only(db.clone()))))
        }
        None => tracing::error!("collab serve oracle not installed: catalog not open"),
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

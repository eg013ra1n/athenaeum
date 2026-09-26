//! Fixtures shared by every wave-3 `api::collab_live` test module (Tasks
//! 7-18, per-task rulings): a signed-in [`ServiceContext`] wired to a
//! [`FakeHub`], with a real relay-disabled iroh node bound and the
//! Collaboration root mounted. `signed_in_rig` is the one entry point;
//! `collab_root`/`my_device`/`seed_replica_file` build on it.
//!
//! P1 headless rule: this module (like the rest of `api::collab_live`) is
//! gated on `render`+`solver` — a test elsewhere that imports it must carry
//! the same gate.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::api::PathPolicy;
use crate::collab::fake_hub::FakeHub;
use crate::services::ServiceContext;
use crate::sharing::iroh::node::SharedIrohNode;

/// The one project every fixture built here seeds: `acc-me` is its sole,
/// coordinating, `send_receive` member.
pub(crate) const PID: &str = "p1";

/// A signed-in `ServiceContext` + `FakeHub` + a real relay-disabled iroh node
/// with the Collaboration root (`<tmp>/Collab`) mounted, and project [`PID`]
/// seeded both on the hub and in the local cache. The returned `TempDir`
/// must be kept alive for as long as `ctx`/`hub` are used.
pub(crate) async fn signed_in_rig() -> (tempfile::TempDir, ServiceContext, FakeHub) {
    let (tmp, ctx, hub) = signed_in_rig_no_root().await;
    let requested = tmp.path().join("Collab");
    std::fs::create_dir_all(&requested).unwrap();
    crate::api::scan_roots::set_collaboration_dir(
        &ctx,
        requested.to_string_lossy().to_string(),
        &PathPolicy::AllowAll,
    )
    .await
    .expect("designate the Collaboration root");
    (tmp, ctx, hub)
}

/// As [`signed_in_rig`], but WITHOUT designating a Collaboration root — for
/// a test that needs to set up its own folder (a pre-existing marker naming
/// another device, say) before anything is designated, e.g. the device
/// replace / reinstall tests.
pub(crate) async fn signed_in_rig_no_root() -> (tempfile::TempDir, ServiceContext, FakeHub) {
    let hub = FakeHub::start().await;
    let (tmp, ctx) = crate::api::collab_exchange::test_support::test_ctx();
    crate::api::collab_exchange::test_support::wire_hub(&ctx, &hub.uri(), "tok");

    let dirs = crate::api::sync::sync_dirs(&ctx).unwrap();
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
    *ctx.iroh_node.lock().await = Some(Arc::clone(&node));

    let my_pubkey = crate::api::account::own_device_id(&ctx).unwrap();
    hub.add_account("tok", "acc-me", "Me", &my_pubkey, None);
    hub.add_project(PID, "m31", &[("acc-me", "send_receive", true)], false);

    seed_local_project(&ctx, &my_pubkey);

    (tmp, ctx, hub)
}

/// The local `collab_projects` cache row [`signed_in_rig`] needs so
/// `db::collab_frames::project_ids` finds [`PID`] (the hub's own project
/// membership is a separate concern, seeded by `FakeHub::add_project`).
fn seed_local_project(ctx: &ServiceContext, my_pubkey: &str) {
    use crate::db::collab::{upsert_project, CollabProjectRow};

    let members_json = serde_json::json!([
        {
            "accountId": "acc-me",
            "displayName": "Me",
            "dataRole": "send_receive",
            "coordinator": true,
            "nodes": [my_pubkey],
        }
    ])
    .to_string();

    let db = crate::api::db(ctx).unwrap();
    let conn = db.conn();
    upsert_project(
        &conn,
        &CollabProjectRow {
            project_id: PID.to_string(),
            slug: "m31".to_string(),
            title: "M31".to_string(),
            data_role: "send_receive".to_string(),
            is_coordinator: true,
            require_approval: false,
            pending_frames: 0,
            project_status: "active".to_string(),
            target_name: "M31".to_string(),
            target_ra_deg: 10.68,
            target_dec_deg: 41.27,
            target_radius_deg: 1.5,
            membership_version: 1,
            snapshot_payload_b64: "x".to_string(),
            snapshot_signature_b64: "x".to_string(),
            members_json,
            thresholds_version: None,
            thresholds_rules_json: None,
            gov_caps_json: "[]".into(),
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

/// The designated Collaboration root — panics if [`signed_in_rig`] did not
/// set one up.
pub(crate) fn collab_root(ctx: &ServiceContext) -> PathBuf {
    PathBuf::from(
        crate::api::scan_roots::get_collaboration_dir(ctx)
            .unwrap()
            .expect("a Collaboration root was designated"),
    )
}

/// This context's own device id, as the hub encodes it (standard base64 of
/// the device pubkey — P3).
pub(crate) async fn my_device(ctx: &ServiceContext) -> String {
    crate::api::account::own_device_id(ctx).unwrap()
}

/// Publish a small, real, byte-exact frame on the hub as `acc-me` (with its
/// real blake3/xxh3/size, not the fake's synthetic placeholders), land it in
/// the local cache exactly as a manifest catch-up would
/// (`upsert_from_manifest`), and drop its bytes at
/// `<root>/m31/other/<uuid>.fits` — as if the OLD device (the one about to
/// be replaced) had already landed it there. Returns `(project_id,
/// frame_uuid, path)`.
pub(crate) async fn seed_replica_file(
    ctx: &ServiceContext,
    hub: &FakeHub,
    root: &Path,
) -> (String, String, PathBuf) {
    let uuid = "f-old-1";
    let bytes = b"a tiny replica frame, just enough bytes to hash".to_vec();
    let blake3 = blake3::hash(&bytes).to_hex().to_string();
    let xxh3 = format!("{:016x}", xxhash_rust::xxh3::xxh3_64(&bytes));

    hub.seed_frames(PID, "acc-me", &[uuid], "published");
    hub.update_frame(PID, uuid, |f| {
        f.blake3 = blake3;
        f.byte_size = bytes.len() as i64;
        f.xxh3 = xxh3;
    });
    let view = hub.frame(PID, uuid).expect("frame just seeded");

    {
        let db = crate::api::db(ctx).unwrap();
        let conn = db.conn();
        crate::db::collab_frames::upsert_from_manifest(&conn, PID, &view).unwrap();
    }

    let dest_dir = root.join("m31").join("other");
    std::fs::create_dir_all(&dest_dir).unwrap();
    let path = dest_dir.join(format!("{uuid}.fits"));
    std::fs::write(&path, &bytes).unwrap();

    (PID.to_string(), uuid.to_string(), path)
}

/// A signed-in rig with `n` held replica frames (Task 9): real files ABOVE
/// the store's 16 KiB inline threshold (fix round 1 — like real FITS frames,
/// so they are imported by reference) under `<root>/m31/other/`,
/// published on the hub by a second account (`acc-o`) with their real
/// hashes, cached through `upsert_from_manifest`, seeded into the collab
/// store and moved to `held` through `set_local_state` (so each carries its
/// outbox `add`). Until Task 11's landing exists the files are placed
/// directly.
pub(crate) struct LandedRig {
    pub _tmp: tempfile::TempDir,
    pub ctx: Arc<ServiceContext>,
    pub hub: FakeHub,
    pub node: Arc<SharedIrohNode>,
    pub root: PathBuf,
    /// `(project_id, frame_uuid, landed path)`, in uuid order.
    pub frames: Vec<(String, String, PathBuf)>,
    /// The local checks the node's serve oracle queued (Task 10): the
    /// receiving half of the [`DbServeOracle`](crate::api::collab_live::serve_oracle::DbServeOracle)
    /// channel [`landed_rig`] installs.
    pub checks: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<(String, String)>>,
}

impl LandedRig {
    /// The blake3 of frame `i` as the catalog records it (its current version).
    pub(crate) fn hash_of(&self, i: usize) -> iroh_blobs::Hash {
        let (pid, uuid, _) = &self.frames[i];
        let row =
            crate::db::collab_frames::get(&crate::api::db(&self.ctx).unwrap().conn(), pid, uuid)
                .unwrap()
                .expect("a landed row");
        row.blake3.parse().expect("a blake3 hex")
    }

    /// The next local check the serve oracle queued, or `None` after 5 s.
    pub(crate) async fn next_local_check(&self) -> Option<(String, String)> {
        let mut rx = self.checks.lock().await;
        tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .ok()
            .flatten()
    }

    /// A storage engine on this rig's root, with the marker this device
    /// recorded at designation.
    pub(crate) fn engine(&self) -> crate::api::collab_live::storage_task::StorageEngine {
        let me = crate::api::account::own_device_id(&self.ctx).unwrap();
        let recorded = crate::db::collab_live::recorded_store_marker(
            &crate::api::db(&self.ctx).unwrap().conn(),
        )
        .unwrap();
        assert!(recorded.is_some(), "designation recorded the store marker");
        let guard = Arc::new(crate::collab::storage::marker::StoreGuard::new(
            self.root.clone(),
            me,
            recorded,
        ));
        crate::api::collab_live::storage_task::StorageEngine::start(
            Arc::clone(&self.ctx),
            Arc::clone(&self.node),
            guard,
        )
    }
}

pub(crate) async fn landed_rig(n: usize) -> LandedRig {
    let (tmp, ctx, hub) = signed_in_rig().await;
    let root = collab_root(&ctx);
    let node = ctx.iroh_node.lock().await.clone().expect("a bound node");
    assert!(node.collab_store().is_some(), "the collab store is mounted");
    let dir = root.join("m31").join("other");
    std::fs::create_dir_all(&dir).unwrap();

    let uuids: Vec<String> = (0..n).map(|i| format!("f{i:02}")).collect();
    let mut frames = Vec::with_capacity(n);
    for uuid in &uuids {
        let bytes = format!("replica frame {uuid}: pixels ")
            .repeat(1024)
            .into_bytes();
        debug_assert!(bytes.len() > 16 * 1024);
        let path = dir.join(format!("{uuid}.fits"));
        land_frame(&ctx, &hub, &node, uuid, &path, &bytes).await;
        frames.push((PID.to_string(), uuid.clone(), path));
    }
    let ctx = Arc::new(ctx);
    // The live session's serve oracle (Task 10), on a guard that checked the
    // marker designation wrote.
    let me = crate::api::account::own_device_id(&ctx).unwrap();
    let recorded =
        crate::db::collab_live::recorded_store_marker(&crate::api::db(&ctx).unwrap().conn())
            .unwrap();
    let guard = Arc::new(crate::collab::storage::marker::StoreGuard::new(
        root.clone(),
        me,
        recorded,
    ));
    assert!(
        guard.check_now().serving(),
        "the designated root is available"
    );
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    node.set_collab_serve_oracle(Some(Arc::new(
        crate::api::collab_live::serve_oracle::DbServeOracle::new(Arc::clone(&ctx), guard, tx),
    )));
    LandedRig {
        _tmp: tmp,
        ctx,
        hub,
        node,
        root,
        frames,
        checks: tokio::sync::Mutex::new(rx),
    }
}

/// A relay-disabled node with no catalog behind it (a fetcher in the
/// two-node tests), in its own temp dir.
pub(crate) struct BareNode {
    pub node: Arc<SharedIrohNode>,
    _tmp: tempfile::TempDir,
}

impl std::ops::Deref for BareNode {
    type Target = Arc<SharedIrohNode>;
    fn deref(&self) -> &Self::Target {
        &self.node
    }
}

pub(crate) async fn bare_node() -> BareNode {
    let tmp = tempfile::tempdir().unwrap();
    let node = SharedIrohNode::bind_with(
        tmp.path(),
        tmp.path(),
        iroh::RelayMode::Disabled,
        crate::sharing::iroh::node::NodeOptions::default(),
    )
    .await
    .expect("bind relay-disabled node");
    BareNode { node, _tmp: tmp }
}

/// Relay-disabled nodes have no discovery: exchange addresses.
pub(crate) async fn pair(a: &Arc<SharedIrohNode>, b: &Arc<SharedIrohNode>) {
    for n in [a, b] {
        n.handle(crate::sharing::iroh::node::Role::Out)
            .start()
            .await
            .unwrap();
    }
    a.add_peer(b.endpoint_addr());
    b.add_peer(a.endpoint_addr());
}

/// An empty in-memory blob store to fetch into.
pub(crate) fn scratch_store() -> iroh_blobs::api::Store {
    iroh_blobs::store::mem::MemStore::new().into()
}

/// Publish `uuid` on the hub as `acc-o` with `bytes`' real hashes, write
/// the file at `path`, cache the row, seed it and move it to `held` — the
/// landing [`landed_rig`] fakes until Task 11's exists.
pub(crate) async fn land_frame(
    ctx: &ServiceContext,
    hub: &FakeHub,
    node: &SharedIrohNode,
    uuid: &str,
    path: &Path,
    bytes: &[u8],
) {
    hub.seed_frames(PID, "acc-o", &[uuid], "published");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
    let blake3 = blake3::hash(bytes).to_hex().to_string();
    let xxh3 = format!("{:016x}", xxhash_rust::xxh3::xxh3_64(bytes));
    let file_name = path.file_name().unwrap().to_string_lossy().to_string();
    hub.update_frame(PID, uuid, |f| {
        f.blake3 = blake3;
        f.byte_size = bytes.len() as i64;
        f.xxh3 = xxh3;
        f.file_name = file_name;
    });
    let view = hub.frame(PID, uuid).expect("frame just seeded");
    {
        let conn = crate::api::db(ctx).unwrap().conn();
        crate::db::collab_frames::upsert_from_manifest(&conn, PID, &view).unwrap();
    }
    node.seed_project_frame(PID, uuid, view.content_version, path)
        .await
        .expect("seed the landed frame");
    let conn = crate::api::db(ctx).unwrap().conn();
    let stamp = crate::collab::storage::sweep::Stamp::of(&std::fs::metadata(path).unwrap());
    crate::db::collab_frames::update_landed_path(&conn, PID, uuid, &path.to_string_lossy())
        .unwrap();
    crate::db::collab_frames::set_size_mtime_seen(&conn, PID, uuid, &stamp.encode()).unwrap();
    crate::db::collab_frames::set_local_state(
        &conn,
        PID,
        uuid,
        crate::db::collab_frames::LocalState::Held,
    )
    .unwrap();
}

/// Move `path`'s mtime `secs_forward` seconds past now (beyond the 2 s
/// tolerance for anything ≥ 3).
pub(crate) fn set_mtime(path: &Path, secs_forward: u64) {
    let t = std::time::SystemTime::now() + std::time::Duration::from_secs(secs_forward);
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

/// Flip one byte, keep the size, bump the mtime — an in-place edit.
pub(crate) fn overwrite_same_size(path: &Path) {
    let mut bytes = std::fs::read(path).unwrap();
    bytes[0] ^= 0xff;
    std::fs::write(path, &bytes).unwrap();
    set_mtime(path, 10);
}

/// Frames of [`fetch_rig`]: above the collab store's 16 KiB inline limit, so
/// a fetched blob is OWNED by the store and lands by the DIRECT export
/// (Task 11 ruling R1: the landing tests must run on that path).
pub(crate) const FETCH_FRAME_BYTES: usize = 64 * 1024;

/// Task 11's two-node landing rig: a signed-in receiver (`ctx`, `node`, the
/// Collaboration root `root`) whose manifest is synced from the fake hub
/// (rows `wanted`), and a relay-disabled provider with its own collab store
/// holding every published version's bytes. [`fetch_blob`](Self::fetch_blob)
/// is one `execute_get` from the provider over the collab ALPN into the
/// receiver's collab store, under the frame's in-flight tag; it remembers
/// the row as it stood then — the row a landing carries.
pub(crate) struct FetchRig {
    pub _tmp: tempfile::TempDir,
    pub ctx: Arc<ServiceContext>,
    pub hub: FakeHub,
    pub node: Arc<SharedIrohNode>,
    pub root: PathBuf,
    pub provider: BareNode,
    _provider_root: tempfile::TempDir,
    pub guard: crate::collab::storage::marker::StoreGuard,
    pub hooks: crate::sharing::iroh::blobs::ExportHooks,
    uuids: Vec<String>,
    /// Per frame, the bytes of every published version (index 0 = v1).
    versions: std::sync::Mutex<Vec<Vec<Vec<u8>>>>,
    /// Per frame, the row as it stood at its last [`fetch_blob`](Self::fetch_blob).
    in_flight: std::sync::Mutex<Vec<Option<crate::db::collab_frames::LocalFrameRow>>>,
}

/// `n` frames of [`FETCH_FRAME_BYTES`], published and fetched (v1).
pub(crate) async fn fetch_rig(n: usize) -> FetchRig {
    fetch_rig_sized(n, FETCH_FRAME_BYTES).await
}

/// [`fetch_rig`] with frames of `size` bytes (≤ 16 KiB: inline blobs).
pub(crate) async fn fetch_rig_sized(n: usize, size: usize) -> FetchRig {
    let (tmp, ctx, hub) = signed_in_rig().await;
    let root = collab_root(&ctx);
    let node = ctx.iroh_node.lock().await.clone().expect("a bound node");
    let provider = bare_node().await;
    let provider_root = tempfile::tempdir().unwrap();
    provider
        .set_collab_root(Some(provider_root.path()))
        .await
        .expect("mount the provider's collab store");
    pair(&node, &provider).await;
    let me = crate::api::account::own_device_id(&ctx).unwrap();
    let recorded =
        crate::db::collab_live::recorded_store_marker(&crate::api::db(&ctx).unwrap().conn())
            .unwrap();
    let guard = crate::collab::storage::marker::StoreGuard::new(root.clone(), me, recorded);
    assert!(
        guard.check_now().fetching(),
        "the designated root is available"
    );
    let uuids: Vec<String> = (0..n).map(|i| format!("f{i:02}")).collect();
    let rig = FetchRig {
        _tmp: tmp,
        ctx: Arc::new(ctx),
        hub,
        node,
        root,
        provider,
        _provider_root: provider_root,
        guard,
        hooks: Default::default(),
        versions: std::sync::Mutex::new(vec![Vec::new(); n]),
        in_flight: std::sync::Mutex::new(vec![None; n]),
        uuids,
    };
    for i in 0..n {
        let uuid = rig.uuids[i].clone();
        let bytes = FetchRig::pattern(&uuid, 1, size);
        rig.provide(&bytes).await;
        rig.hub.seed_frames(PID, "acc-o", &[&uuid], "published");
        rig.publish(i, bytes, false);
    }
    for i in 0..n {
        rig.fetch_blob(i).await;
    }
    rig
}

impl FetchRig {
    /// `size` bytes of frame `uuid`, version `version` — distinct per both.
    pub(crate) fn pattern(uuid: &str, version: i32, size: usize) -> Vec<u8> {
        format!("frame {uuid} v{version}: pixels ")
            .into_bytes()
            .into_iter()
            .cycle()
            .take(size)
            .collect()
    }

    pub(crate) fn frame(&self, i: usize) -> (String, String) {
        (PID.to_string(), self.uuids[i].clone())
    }

    pub(crate) fn receiver_store(&self) -> iroh_blobs::api::Store {
        self.node
            .collab_store()
            .expect("the receiver's collab store")
    }

    pub(crate) fn row(&self, i: usize) -> crate::db::collab_frames::LocalFrameRow {
        crate::db::collab_frames::get(
            &crate::api::db(&self.ctx).unwrap().conn(),
            PID,
            &self.uuids[i],
        )
        .unwrap()
        .expect("a cached row")
    }

    pub(crate) fn bytes(&self, i: usize, version: usize) -> Vec<u8> {
        self.versions.lock().unwrap()[i][version - 1].clone()
    }

    pub(crate) fn v1_bytes(&self, i: usize) -> Vec<u8> {
        self.bytes(i, 1)
    }

    pub(crate) fn v2_bytes(&self, i: usize) -> Vec<u8> {
        self.bytes(i, 2)
    }

    pub(crate) fn v2_hash(&self, i: usize) -> iroh_blobs::Hash {
        iroh_blobs::Hash::new(self.v2_bytes(i))
    }

    /// Put `bytes` into the provider's collab store (tagged, so they stay).
    async fn provide(&self, bytes: &[u8]) {
        let store = self.provider.collab_store().expect("provider store");
        let tt = store
            .blobs()
            .add_bytes(bytes.to_vec())
            .temp_tag()
            .await
            .unwrap();
        let hash = tt.hash();
        store
            .tags()
            .set(format!("test/{hash}"), iroh_blobs::HashAndFormat::raw(hash))
            .await
            .unwrap();
    }

    /// Publish `bytes` as frame `i`'s next version on the hub (or its first,
    /// `bump = false`) and sync the receiver's manifest row.
    fn publish(&self, i: usize, bytes: Vec<u8>, bump: bool) {
        let uuid = self.uuids[i].clone();
        let blake3 = blake3::hash(&bytes).to_hex().to_string();
        let xxh3 = format!("{:016x}", xxhash_rust::xxh3::xxh3_64(&bytes));
        let len = bytes.len() as i64;
        self.hub.update_frame(PID, &uuid, |f| {
            if bump {
                f.content_version += 1;
            }
            f.blake3 = blake3;
            f.byte_size = len;
            f.xxh3 = xxh3;
        });
        self.versions.lock().unwrap()[i].push(bytes);
        let view = self.hub.frame(PID, &uuid).expect("frame published");
        let conn = crate::api::db(&self.ctx).unwrap().conn();
        crate::db::collab_frames::upsert_from_manifest(&conn, PID, &view).unwrap();
    }

    /// The provider re-versions frame `i` with new bytes of the same size;
    /// the receiver's manifest follows (a held row goes back to `wanted`,
    /// its old file untouched).
    pub(crate) async fn publish_new_version(&self, i: usize) {
        let next = self.versions.lock().unwrap()[i].len() as i32 + 1;
        let size = self.v1_bytes(i).len();
        let bytes = Self::pattern(&self.uuids[i], next, size);
        self.publish_version_with(i, bytes).await;
    }

    /// [`publish_new_version`](Self::publish_new_version) with given bytes.
    pub(crate) async fn publish_version_with(&self, i: usize, bytes: Vec<u8>) {
        self.provide(&bytes).await;
        self.publish(i, bytes, true);
    }

    /// Fetch frame `i`'s current bytes from the provider into the receiver's
    /// collab store under the frame's in-flight tag, and remember the row.
    pub(crate) async fn fetch_blob(&self, i: usize) {
        let row = self.row(i);
        let hash: iroh_blobs::Hash = row.blake3.parse().unwrap();
        let conn = self
            .node
            .endpoint()
            .connect(
                self.provider.endpoint_addr(),
                crate::sharing::iroh::COLLAB_BLOBS_ALPN,
            )
            .await
            .expect("dial the provider on the collab ALPN");
        let store = self.receiver_store();
        store
            .remote()
            .execute_get(conn, iroh_blobs::protocol::GetRequest::blob(hash))
            .await
            .expect("fetched");
        store
            .tags()
            .set(
                crate::api::collab_live::landing::project_frame_in_flight_tag(
                    PID,
                    &row.frame_uuid,
                    row.content_version,
                ),
                iroh_blobs::HashAndFormat::raw(hash),
            )
            .await
            .unwrap();
        self.in_flight.lock().unwrap()[i] = Some(row);
    }

    /// The manifest moved while the bytes were in flight (a version bump
    /// written straight into the receiver's row).
    pub(crate) fn bump_manifest_version_locally(&self, i: usize) {
        crate::api::db(&self.ctx)
            .unwrap()
            .conn()
            .execute(
                "UPDATE project_frames_local SET content_version = content_version + 1
                 WHERE project_id = ?1 AND frame_uuid = ?2",
                rusqlite::params![PID, self.uuids[i]],
            )
            .unwrap();
    }

    /// Land frame `i` with the row its last fetch carried: `Ok(path)` for
    /// `Landed::Yes`, `Err(landed)` otherwise.
    pub(crate) async fn land(
        &self,
        i: usize,
    ) -> Result<PathBuf, crate::api::collab_live::landing::Landed> {
        use crate::api::collab_live::landing::{land_frame, Landed, LandingEnv};
        let row = self.in_flight.lock().unwrap()[i]
            .clone()
            .expect("fetch_blob first");
        let hash: iroh_blobs::Hash = row.blake3.parse().unwrap();
        let project =
            crate::db::collab::get_project(&crate::api::db(&self.ctx).unwrap().conn(), PID)
                .unwrap()
                .expect("the project row");
        let store = self.receiver_store();
        let started_at = crate::sync::now_iso();
        let env = LandingEnv {
            ctx: &self.ctx,
            node: &self.node,
            store: &store,
            project: &project,
            collab_root: &self.root,
            guard: &self.guard,
            started_at: &started_at,
            hooks: &self.hooks,
        };
        match land_frame(&env, &row, hash).await {
            Landed::Yes(p) => Ok(p),
            other => Err(other),
        }
    }
}

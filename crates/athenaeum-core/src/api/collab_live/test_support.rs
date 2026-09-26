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
}

impl LandedRig {
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
    LandedRig {
        _tmp: tmp,
        ctx: Arc::new(ctx),
        hub,
        node,
        root,
        frames,
    }
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

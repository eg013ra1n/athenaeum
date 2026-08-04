//! Acceptance floor for task A7: the four named receive-side tests.
//!
//! `ingest_lands_files_and_rows`, `duplicate_delivery_single_row_but_acked`, and
//! `primary_wins_metadata` exercise [`ingest_package`] directly against a catalog
//! connection (deterministic, no timing). `ack_replay_from_receipt_log` drives
//! the full [`SyncReceiver`] over the in-process
//! [`LoopbackTransport`](crate::sharing::loopback::LoopbackTransport): a re-sent
//! announce for a fully-receipted package must re-ack from the log without
//! re-fetching or re-ingesting.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use tempfile::TempDir;
use tokio::sync::mpsc::Receiver;
use tokio::time::Instant;

use crate::db::schema::init_db;
use crate::events::NullEmitter;
use crate::fits_writer::{write_fits_f32, Card, CardValue};
use crate::models::{Frame, ImageType};
use crate::package::{self, ManifestRecord, PayloadKind, MANIFEST_VERSION};
use crate::sharing::loopback::{FaultPlan, LoopbackNetwork, LoopbackTransport};
use crate::sharing::types::{
    FrameReceipt, NodeId, PackageAnnounce, PackageId, PackageLayout, ReceiptOutcome, TransportEvent,
};
use crate::sharing::SharingTransport;

use super::ingest::{ingest_package, IngestConn};
use super::node_id_hex;
use super::receiver::{IncomingResolver, SyncReceiver};
use super::store::{count_satisfied_receipts, insert_receipt, CatalogSyncStore, SyncStore};
use super::{StandaloneSyncStore, SyncConfig, SyncEngine};

const ORIGIN_DEVICE: &str = "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899";
const PEER_HEX: &str = "1122334455667788112233445566778811223344556677881122334455667788";

/// A realistic `models::Frame` snapshot for a manifest, with an explicit uuid +
/// updated_at (the identity + primary-wins comparison anchors).
fn fixture_frame(uuid: &str, object: &str, updated_at: &str) -> Frame {
    let date_obs: DateTime<Utc> = "2026-01-15T22:30:00Z".parse().unwrap();
    Frame {
        object: Some(object.to_string()),
        date_obs: Some(date_obs),
        telescop: Some("APM107".to_string()),
        instrume: Some("ASI2600MM".to_string()),
        exptime: Some(300.0),
        filter: Some("Ha".to_string()),
        imagetyp: Some(ImageType::Light),
        gain: Some(100.0),
        offset: Some(50.0),
        binning: Some("1x1".to_string()),
        naxis1: Some(4),
        naxis2: Some(4),
        uuid: Some(uuid.to_string()),
        updated_at: Some(updated_at.to_string()),
        ..Default::default()
    }
}

/// Write a minimal real FITS file at `path` (4x4 mono, no user cards).
fn write_fits(path: &Path) {
    write_fits_with_cards(path, &[]);
}

/// Write a minimal real FITS file at `path` carrying `cards` as extra header
/// cards — lets a test prove the receiver reads the LANDED FILE's own header
/// and not just the manifest snapshot.
fn write_fits_with_cards(path: &Path, cards: &[Card]) {
    write_fits_f32(path, 4, 4, 1, &[0.0f32; 16], cards).unwrap();
}

/// Write a minimal real FITS file at `path` with every pixel set to `val` — lets
/// a test build two payloads with genuinely different full-content hashes
/// (`write_fits` alone would make every fixture byte-identical).
fn write_fits_val(path: &Path, val: f32) {
    write_fits_f32(path, 4, 4, 1, &[val; 16], &[]).unwrap();
}

/// Build a one-frame fixture package under `root` and return `(pkg_dir, announce)`.
/// The payload is a real FITS file; the manifest carries `frame_uuid` and a
/// serialized `frame_meta` snapshot whose `object`/`updated_at` are `object` /
/// `updated_at`.
fn build_fixture_package(
    root: &Path,
    frame_uuid: &str,
    filename: &str,
    object: &str,
    updated_at: &str,
) -> (PathBuf, PackageAnnounce) {
    build_fixture_package_with_cards(root, frame_uuid, filename, object, updated_at, &[])
}

/// As [`build_fixture_package`], but the payload FITS carries `cards` as extra
/// header cards. The manifest snapshot is unaffected — so a test can deliver a
/// snapshot that is silent about a field the landed file does declare.
fn build_fixture_package_with_cards(
    root: &Path,
    frame_uuid: &str,
    filename: &str,
    object: &str,
    updated_at: &str,
    cards: &[Card],
) -> (PathBuf, PackageAnnounce) {
    let src_dir = root.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let src = src_dir.join(filename);
    write_fits_with_cards(&src, cards);

    let byte_size = std::fs::metadata(&src).unwrap().len();
    let xxh3 = package::xxh3_full_file(&src).unwrap();
    let frame = fixture_frame(frame_uuid, object, updated_at);
    let record = ManifestRecord {
        v: MANIFEST_VERSION,
        frame_uuid: frame_uuid.to_string(),
        origin_catalog_uuid: "catalog-uuid".to_string(),
        origin_device: ORIGIN_DEVICE.to_string(),
        payload_kind: PayloadKind::RawFrame,
        rel_path: filename.to_string(),
        byte_size,
        xxh3,
        frame_meta: serde_json::to_value(&frame).unwrap(),
        analysis: None,
        app_version: "test".to_string(),
        project: None,
    };

    let pkg_dir = root.join(format!("pkg-{frame_uuid}"));
    let announce = package::write_package(&pkg_dir, vec![(src, record)]).unwrap();
    (pkg_dir, announce)
}

/// Builds a one-file package with an explicit nested `rel_path` and a decoy
/// `origin_device`, so a test can prove landing mirrors `rel_path` under the
/// AUTHENTICATED peer (not the manifest's origin_device). Returns (pkg_dir, announce).
fn build_nested_package(
    root: &Path,
    frame_uuid: &str,
    rel_path: &str,
    decoy_origin_device: &str,
) -> (PathBuf, PackageAnnounce) {
    let src_dir = root.join("src-nested");
    std::fs::create_dir_all(&src_dir).unwrap();
    let src = src_dir.join("payload.fits");
    write_fits(&src);
    let byte_size = std::fs::metadata(&src).unwrap().len();
    let xxh3 = package::xxh3_full_file(&src).unwrap();
    let record = ManifestRecord {
        v: MANIFEST_VERSION,
        frame_uuid: frame_uuid.to_string(),
        origin_catalog_uuid: frame_uuid.to_string(),
        origin_device: decoy_origin_device.to_string(),
        payload_kind: PayloadKind::RawFrame,
        rel_path: rel_path.to_string(),
        byte_size,
        xxh3,
        // A full, valid Frame snapshot (the receiver deserializes frame_meta into
        // `models::Frame`, which requires more than a bare `object`); object is
        // "M31" for readability but the landing path is driven by `rel_path`.
        frame_meta: serde_json::to_value(fixture_frame(
            frame_uuid,
            "M31",
            "2026-01-16T10:00:00.000Z",
        ))
        .unwrap(),
        analysis: None,
        app_version: "test".to_string(),
        project: None,
    };
    let pkg_dir = root.join(format!("pkg-{frame_uuid}"));
    let announce = package::write_package(&pkg_dir, vec![(src, record)]).unwrap();
    (pkg_dir, announce)
}

#[tokio::test]
async fn ingest_mirrors_rel_path_under_authenticated_peer_slug() {
    let tmp = TempDir::new().unwrap();
    let catalog_path = tmp.path().join("catalog.db");
    let assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
    let sync_dir = tmp.path().join("sync");
    let incoming = sync_dir.join("incoming");
    let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());

    let net = LoopbackNetwork::new();
    let sender: Arc<LoopbackTransport> = Arc::new(net.endpoint());
    let receiver_ep: Arc<LoopbackTransport> = Arc::new(net.endpoint());
    let receiver_node: NodeId = receiver_ep.node_id();
    let sender_node: NodeId = sender.node_id();
    sender.start().await.unwrap();
    let mut sender_events = sender.events().await;

    let (_info, _handle) = SyncReceiver::spawn(
        Arc::clone(&store),
        sync_dir.clone(),
        fixed_resolver(incoming.clone()),
        super::allow_all_peers(),
        Default::default(), // no project announce gate in this test
        Arc::new(crate::sync::InboundControl::new()),
        Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
        Arc::new(NullEmitter),
    )
    .await
    .unwrap();

    // Nested rel_path + a decoy origin_device that must NOT appear in the path.
    let (pkg_dir, announce) = build_nested_package(
        tmp.path(),
        "frame-nested-1",
        "M31/2026-07-10/lights/L_0001.fits",
        "deadbeefdeadbeefdeadbeefdeadbeef", // decoy origin_device
    );
    sender.serve(&announce, &pkg_dir, None).await.unwrap();
    sender
        .announce(receiver_node, &announce, "", "", &[], PackageLayout::Batch)
        .await
        .unwrap();
    let receipts = wait_for_ack(
        &mut sender_events,
        &announce.package_id.0,
        Duration::from_secs(5),
    )
    .await;
    assert!(matches!(receipts[0].outcome, ReceiptOutcome::Ingested));

    // The slug is the authenticated sender node id, sanitized (NOT the decoy).
    let slug = super::ingest::sanitize_slug(&node_id_hex(&sender_node));
    let expected = incoming
        .join(&slug)
        .join("M31/2026-07-10/lights/L_0001.fits");
    assert!(
        expected.exists(),
        "file must mirror rel_path under the peer slug: {}",
        expected.display()
    );

    // The decoy origin_device must NOT be a folder anywhere under incoming.
    let decoy_slug = super::ingest::sanitize_slug("deadbeefdeadbeefdeadbeefdeadbeef");
    assert!(
        !incoming.join(&decoy_slug).exists(),
        "manifest-declared origin_device must NOT drive the landing path"
    );

    // The catalog row points at the mirrored path.
    let c = assert_db.conn();
    let path: String = c
        .query_row("SELECT path FROM files LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert!(
        path.ends_with("M31/2026-07-10/lights/L_0001.fits"),
        "catalog path mirrors rel_path: {path}"
    );
}

#[test]
fn sanitize_slug_is_path_safe() {
    assert_eq!(super::ingest::sanitize_slug("Studio Mac"), "studio-mac");
    assert_eq!(super::ingest::sanitize_slug("../../etc"), "etc"); // separators/dots → safe
    assert_eq!(super::ingest::sanitize_slug("a/b\\c:d"), "a-b-c-d");
    assert_eq!(super::ingest::sanitize_slug(""), "node");
    assert_eq!(super::ingest::sanitize_slug("!!!"), "node");
    // hex node id stays hex, capped.
    let s = super::ingest::sanitize_slug(&"ab".repeat(32));
    assert!(s.len() <= 24 && s.chars().all(|c| c.is_ascii_hexdigit()));
}

/// Set the cached node-id-hex → device-name map on `conn` (the source
/// [`resolve_sender_slug`] reads).
fn set_device_names(conn: &Connection, pairs: &[(&str, &str)]) {
    let map: std::collections::HashMap<String, String> = pairs
        .iter()
        .map(|(h, n)| (h.to_string(), n.to_string()))
        .collect();
    crate::db::set_setting(
        conn,
        crate::settings::keys::SYNC_DEVICE_NAMES,
        &serde_json::to_string(&map).unwrap(),
    )
    .unwrap();
}

/// The landing slug prefers the sender's CURRENT friendly device name from the
/// cached map (sanitized with the shared `sanitize_for_filename`), and falls back
/// to the hex slug for a peer not in the map.
#[test]
fn resolve_sender_slug_prefers_cached_device_name() {
    let conn = catalog_conn();

    // No cache yet → hex slug (pre-2C behavior, pinned).
    assert_eq!(
        super::ingest::resolve_sender_slug(&conn, PEER_HEX),
        super::ingest::sanitize_slug(PEER_HEX)
    );

    // Cache the peer's friendly name → slug becomes the exact sanitized form.
    set_device_names(
        &conn,
        &[(PEER_HEX, "My Mac Book"), (&"ff".repeat(32), "Other")],
    );
    assert_eq!(
        super::ingest::resolve_sender_slug(&conn, PEER_HEX),
        "My_Mac_Book",
        "whitespace → underscore via the shared sanitize_for_filename"
    );

    // A peer absent from the map → hex fallback (never fails).
    let unknown = "aa".repeat(32);
    assert_eq!(
        super::ingest::resolve_sender_slug(&conn, &unknown),
        super::ingest::sanitize_slug(&unknown)
    );
}

/// A cached name that sanitizes to empty (all-reserved chars) must not yield an
/// empty path segment — the hex slug is used instead.
#[test]
fn resolve_sender_slug_falls_back_when_name_sanitizes_empty() {
    let conn = catalog_conn();
    set_device_names(&conn, &[(PEER_HEX, "///")]); // "/"→"_", collapses+trims to ""
    assert_eq!(
        super::ingest::resolve_sender_slug(&conn, PEER_HEX),
        super::ingest::sanitize_slug(PEER_HEX)
    );
}

/// Batch-review finding: a device named ".." (or "." / "  ..  ", which whitespace-
/// collapses to "..") must NOT survive as the slug — `sanitize_for_filename`
/// treats '.' as an ordinary char, so without the dot-trim fix the slug would be
/// literally ".." and `land_payload`'s `incoming_root.join(sender_slug)` would
/// escape one level above the incoming root. Each of these must fall back to the
/// HEX slug, and a normal dotted name must be unaffected.
#[test]
fn resolve_sender_slug_rejects_dot_only_names() {
    let conn = catalog_conn();
    let hex_slug = super::ingest::sanitize_slug(PEER_HEX);

    for dotty in ["..", ".", "  ..  ", "...", "  .  "] {
        set_device_names(&conn, &[(PEER_HEX, dotty)]);
        let slug = super::ingest::resolve_sender_slug(&conn, PEER_HEX);
        assert_eq!(
            slug, hex_slug,
            "device name {dotty:?} must fall back to the hex slug, got {slug:?}"
        );
        assert_ne!(
            slug, "..",
            "slug must never be the literal parent-dir component"
        );
        assert_ne!(
            slug, ".",
            "slug must never be the literal current-dir component"
        );
    }

    // A normal name that merely contains a dot (not dot-only) is unaffected.
    set_device_names(&conn, &[(PEER_HEX, "My.Mac")]);
    assert_eq!(
        super::ingest::resolve_sender_slug(&conn, PEER_HEX),
        "My.Mac"
    );
}

/// End to end through `ingest_package`: a resolver-cached ".." device name must
/// land under the HEX slug directory, INSIDE `incoming_root` — never one level
/// above it. Asserts the landed path has no ".." path component and stays
/// under `incoming`.
#[test]
fn ingest_dot_only_device_name_lands_under_hex_slug_not_above_incoming_root() {
    let tmp = TempDir::new().unwrap();
    let incoming = tmp.path().join("incoming");
    let (pkg_dir, announce) = build_fixture_package(
        tmp.path(),
        "frame-uuid-dotty",
        "L_dotty.fits",
        "M31",
        "2026-01-16T10:00:00.000Z",
    );

    let conn = catalog_conn();
    set_device_names(&conn, &[(PEER_HEX, "..")]);

    let outcome = ingest_package(
        IngestConn::Borrowed(&conn),
        &incoming,
        &pkg_dir,
        &announce,
        PEER_HEX,
        &announce.package_id.0,
        None,
        None,
    )
    .unwrap();
    assert_eq!(outcome.ingested, 1);

    let landed_path: String = conn
        .query_row("SELECT path FROM files LIMIT 1", [], |r| r.get(0))
        .unwrap();
    let landed = Path::new(&landed_path);
    assert!(landed.exists(), "landed file exists on disk");
    assert!(
        landed.starts_with(&incoming),
        "landed file must stay under incoming_root, not escape it: {landed_path}"
    );
    assert!(
        !landed.components().any(|c| c.as_os_str() == ".."),
        "landed path must not contain a '..' component: {landed_path}"
    );
    let hex_slug = super::ingest::sanitize_slug(PEER_HEX);
    assert!(
        landed_path.ends_with(&format!("{hex_slug}/L_dotty.fits")),
        "a dot-only device name falls back to the hex slug: {landed_path}"
    );
}

/// End to end through `ingest_package`: with a cached device name, the payload
/// lands under `<incoming>/<sanitized name>/<rel_path>` and the catalog row
/// points at that path.
#[test]
fn ingest_lands_under_resolved_device_name() {
    let tmp = TempDir::new().unwrap();
    let incoming = tmp.path().join("incoming");
    let (pkg_dir, announce) = build_fixture_package(
        tmp.path(),
        "frame-uuid-named",
        "L_named.fits",
        "M31",
        "2026-01-16T10:00:00.000Z",
    );

    let conn = catalog_conn();
    set_device_names(&conn, &[(PEER_HEX, "My Mac Book")]);

    let outcome = ingest_package(
        IngestConn::Borrowed(&conn),
        &incoming,
        &pkg_dir,
        &announce,
        PEER_HEX,
        &announce.package_id.0,
        None,
        None,
    )
    .unwrap();
    assert_eq!(outcome.ingested, 1);

    let landed_path: String = conn
        .query_row("SELECT path FROM files LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert!(
        Path::new(&landed_path).exists(),
        "landed file exists on disk"
    );
    assert!(
        landed_path.ends_with("My_Mac_Book/L_named.fits"),
        "lands under the sanitized device name, not the hex slug: {landed_path}"
    );
    // The hex-slug dir must NOT exist (the friendly name replaced it).
    let hex_slug = super::ingest::sanitize_slug(PEER_HEX);
    assert!(
        !incoming.join(&hex_slug).exists(),
        "a resolved name replaces the hex slug entirely"
    );
}

/// Build a two-frame fixture package (one `pkg_dir`, one announce/`package_id`
/// covering both frames) with genuinely distinct pixel content per frame — used
/// by the transit-corruption test so corrupting one payload cannot coincidentally
/// collide with the other's hash. Returns `(pkg_dir, [uuid_a, uuid_b], path_of_b_in_pkg_dir)`.
fn build_two_frame_package(root: &Path) -> (PathBuf, [String; 2], PathBuf) {
    let src_dir = root.join("src_pair");
    std::fs::create_dir_all(&src_dir).unwrap();

    let path_a = src_dir.join("L_a.fits");
    write_fits_val(&path_a, 0.0);
    let path_b = src_dir.join("L_b.fits");
    write_fits_val(&path_b, 1.0);

    let uuid_a = "frame-uuid-pair-a".to_string();
    let uuid_b = "frame-uuid-pair-b".to_string();

    let record_of = |path: &Path, uuid: &str, object: &str| ManifestRecord {
        v: MANIFEST_VERSION,
        frame_uuid: uuid.to_string(),
        origin_catalog_uuid: "catalog-uuid".to_string(),
        origin_device: ORIGIN_DEVICE.to_string(),
        payload_kind: PayloadKind::RawFrame,
        rel_path: path.file_name().unwrap().to_str().unwrap().to_string(),
        byte_size: std::fs::metadata(path).unwrap().len(),
        xxh3: package::xxh3_full_file(path).unwrap(),
        frame_meta: serde_json::to_value(&fixture_frame(uuid, object, "2026-01-16T10:00:00.000Z"))
            .unwrap(),
        analysis: None,
        app_version: "test".to_string(),
        project: None,
    };
    let record_a = record_of(&path_a, &uuid_a, "PAIR_A");
    let record_b = record_of(&path_b, &uuid_b, "PAIR_B");

    let pkg_dir = root.join("pkg-pair");
    package::write_package(&pkg_dir, vec![(path_a, record_a), (path_b, record_b)]).unwrap();
    let path_b_in_pkg = pkg_dir.join("L_b.fits");
    (pkg_dir, [uuid_a, uuid_b], path_b_in_pkg)
}

/// Flip a few bytes at `offset` in place — simulates transit/storage
/// corruption of an already-written payload file.
fn corrupt_at(path: &Path, offset: u64) {
    use std::io::{Seek, SeekFrom, Write};
    let mut f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.seek(SeekFrom::Start(offset)).unwrap();
    f.write_all(&[0xDE, 0xAD, 0xBE, 0xEF]).unwrap();
}

/// Build a single-frame package whose payload is `bytes` verbatim (not a real
/// FITS file) — used by the sampling-hash-collision test, which only cares
/// about byte layout, not FITS structure. Header extraction on ingest is
/// tolerant of non-FITS content (logs a warning, inserts an empty header row).
fn build_raw_package(
    root: &Path,
    frame_uuid: &str,
    filename: &str,
    bytes: &[u8],
) -> (PathBuf, PackageAnnounce) {
    let src_dir = root.join(format!("src-{frame_uuid}"));
    std::fs::create_dir_all(&src_dir).unwrap();
    let src = src_dir.join(filename);
    std::fs::write(&src, bytes).unwrap();

    let byte_size = bytes.len() as u64;
    let xxh3 = package::xxh3_full_file(&src).unwrap();
    let frame = fixture_frame(frame_uuid, "RAW", "2026-01-16T10:00:00.000Z");
    let record = ManifestRecord {
        v: MANIFEST_VERSION,
        frame_uuid: frame_uuid.to_string(),
        origin_catalog_uuid: "catalog-uuid".to_string(),
        origin_device: ORIGIN_DEVICE.to_string(),
        payload_kind: PayloadKind::RawFrame,
        rel_path: filename.to_string(),
        byte_size,
        xxh3,
        frame_meta: serde_json::to_value(&frame).unwrap(),
        analysis: None,
        app_version: "test".to_string(),
        project: None,
    };
    let pkg_dir = root.join(format!("pkg-{frame_uuid}"));
    let announce = package::write_package(&pkg_dir, vec![(src, record)]).unwrap();
    (pkg_dir, announce)
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

/// A catalog connection with the full schema (sync tables included).
fn catalog_conn() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    init_db(&conn).unwrap();
    conn
}

#[test]
fn ingest_lands_files_and_rows() {
    let tmp = TempDir::new().unwrap();
    let incoming = tmp.path().join("incoming");
    let (pkg_dir, announce) = build_fixture_package(
        tmp.path(),
        "frame-uuid-1",
        "L_0001.fits",
        "M31",
        "2026-01-16T10:00:00.000Z",
    );

    let conn = catalog_conn();
    let outcome = ingest_package(
        IngestConn::Borrowed(&conn),
        &incoming,
        &pkg_dir,
        &announce,
        PEER_HEX,
        &announce.package_id.0,
        None,
        None,
    )
    .unwrap();

    // Catalog rows created from manifest metadata.
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM files"),
        1,
        "one files row"
    );
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM frames"),
        1,
        "one frames row"
    );
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM fits_header"),
        1,
        "one fits_header row"
    );

    // frames.uuid carries the manifest frame_uuid (so a redelivery dedups).
    let uuid: String = conn
        .query_row("SELECT uuid FROM frames LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(uuid, "frame-uuid-1");
    let object: Option<String> = conn
        .query_row("SELECT object FROM frames LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(object.as_deref(), Some("M31"));

    // File landed under <incoming>/<sender_slug>/<rel_path>. The slug is derived
    // from the AUTHENTICATED peer node id (PEER_HEX), sanitized — never the
    // manifest's origin_device — and the sender's tree (here a flat rel_path) is
    // mirrored verbatim beneath it.
    let landed_path: String = conn
        .query_row("SELECT path FROM files LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert!(
        Path::new(&landed_path).exists(),
        "landed file exists on disk"
    );
    assert!(
        landed_path.contains("incoming"),
        "under incoming root: {landed_path}"
    );
    let slug = super::ingest::sanitize_slug(PEER_HEX);
    assert!(
        landed_path.ends_with(&format!("{slug}/L_0001.fits")),
        "mirrors rel_path under the peer slug: {landed_path}"
    );

    // History + receipt written.
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM sync_history WHERE direction='received' AND outcome='ingested'"
        ),
        1,
        "one received/ingested history row"
    );
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM sync_receipts"),
        1,
        "one receipt row"
    );

    // Receipt reflects an ingest.
    assert_eq!(outcome.ingested, 1);
    assert_eq!(outcome.receipts.len(), 1);
    assert!(matches!(
        outcome.receipts[0].outcome,
        ReceiptOutcome::Ingested
    ));
}

#[test]
fn duplicate_delivery_single_row_but_acked() {
    let tmp = TempDir::new().unwrap();
    let incoming = tmp.path().join("incoming");
    let (pkg_dir, announce1) = build_fixture_package(
        tmp.path(),
        "frame-uuid-2",
        "L_0002.fits",
        "M42",
        "2026-01-16T10:00:00.000Z",
    );

    let conn = catalog_conn();

    // First delivery ingests.
    let out1 = ingest_package(
        IngestConn::Borrowed(&conn),
        &incoming,
        &pkg_dir,
        &announce1,
        PEER_HEX,
        &announce1.package_id.0,
        None,
        None,
    )
    .unwrap();
    assert_eq!(out1.ingested, 1);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 1);

    // Second delivery of the SAME package dir but a fresh announce (the sender
    // mints a new package_id per announce) — dedup by uuid, no new catalog row,
    // receipt = Duplicate.
    let announce2 = PackageAnnounce {
        package_id: crate::sharing::types::PackageId("second-delivery".to_string()),
        ..announce1.clone()
    };
    let out2 = ingest_package(
        IngestConn::Borrowed(&conn),
        &incoming,
        &pkg_dir,
        &announce2,
        PEER_HEX,
        &announce2.package_id.0,
        None,
        None,
    )
    .unwrap();

    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM files"),
        1,
        "still one files row"
    );
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM frames"),
        1,
        "still one frames row"
    );
    assert_eq!(out2.duplicate, 1, "second delivery deduped");
    assert!(
        matches!(out2.receipts[0].outcome, ReceiptOutcome::Duplicate),
        "receipt says Duplicate"
    );
    // The ack still carries a full receipt set (one per frame) so the sender confirms.
    assert_eq!(out2.receipts.len(), 1);
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM sync_history WHERE outcome='duplicate'"
        ),
        1,
        "duplicate history recorded"
    );
}

/// Wiring pin for the defensive CFA back-fill in `insert_ingested_rows`.
///
/// A sender running a build with the CFA-blind frame projection ships a
/// `frame_meta` snapshot whose Bayer fields are erased, while the payload file
/// it sends still declares them in its own header. The receiver must fill the
/// gap from the landed file — otherwise the frame lands mono-looking and every
/// downstream OSC decision (per-channel flat scaling, debayer) is wrong.
///
/// Deleting the `backfill_frame_cfa` call in `insert_ingested_rows` must fail
/// this test and only this test.
#[test]
fn ingest_backfills_cfa_from_the_landed_file_header() {
    let tmp = TempDir::new().unwrap();
    let incoming = tmp.path().join("incoming");
    let (pkg_dir, announce) = build_fixture_package_with_cards(
        tmp.path(),
        "frame-uuid-cfa",
        "OSC_0001.fits",
        "M42",
        "2026-01-16T10:00:00.000Z",
        &[
            Card::new("BAYERPAT", CardValue::Str("RGGB".to_string())).unwrap(),
            Card::new("XBAYROFF", CardValue::Integer(1)).unwrap(),
            Card::new("YBAYROFF", CardValue::Integer(0)).unwrap(),
            Card::new("ROWORDER", CardValue::Str("BOTTOM-UP".to_string())).unwrap(),
        ],
    );

    // Premise: the snapshot serialized into the manifest really is CFA-blind, so
    // anything the frames row ends up carrying can only have come from the
    // landed file's own header.
    let shipped = fixture_frame("frame-uuid-cfa", "M42", "2026-01-16T10:00:00.000Z");
    assert!(
        shipped.bayerpat.is_none()
            && shipped.xbayroff.is_none()
            && shipped.ybayroff.is_none()
            && shipped.roworder.is_none(),
        "fixture premise: the manifest snapshot must carry no CFA fields"
    );

    let conn = catalog_conn();
    ingest_package(
        IngestConn::Borrowed(&conn),
        &incoming,
        &pkg_dir,
        &announce,
        PEER_HEX,
        &announce.package_id.0,
        None,
        None,
    )
    .unwrap();

    let (bayerpat, xbayroff, ybayroff, roworder): (
        Option<String>,
        Option<i64>,
        Option<i64>,
        Option<String>,
    ) = conn
        .query_row(
            "SELECT bayerpat, xbayroff, ybayroff, roworder FROM frames WHERE uuid = 'frame-uuid-cfa'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();

    assert_eq!(bayerpat.as_deref(), Some("RGGB"), "BAYERPAT back-filled from the landed file");
    assert_eq!(xbayroff, Some(1), "XBAYROFF back-filled from the landed file");
    assert_eq!(ybayroff, Some(0), "YBAYROFF back-filled from the landed file");
    assert_eq!(
        roworder.as_deref(),
        Some("BOTTOM-UP"),
        "ROWORDER back-filled from the landed file"
    );
}

#[test]
fn primary_wins_metadata() {
    let tmp = TempDir::new().unwrap();
    let incoming = tmp.path().join("incoming");
    let conn = catalog_conn();

    // Pre-seed a frame with uuid X that the primary has edited AFTER the incoming
    // snapshot (newer updated_at, edited object).
    conn.execute(
        "INSERT INTO files (path, filename, size, modified_at, format, created_at, content_hash)
         VALUES ('/local/edited.fits', 'edited.fits', 100, '2026-01-15T00:00:00Z', 'FITS', '2026-01-15T00:00:00Z', 'localhash')",
        [],
    )
    .unwrap();
    let file_id: i64 = conn
        .query_row("SELECT id FROM files LIMIT 1", [], |r| r.get(0))
        .unwrap();
    conn.execute(
        "INSERT INTO frames (file_id, object, imagetyp, uuid, updated_at)
         VALUES (?1, 'EDITED_ON_PRIMARY', 'Light', 'frame-uuid-3', '2030-01-01T00:00:00.000Z')",
        rusqlite::params![file_id],
    )
    .unwrap();

    // Deliver an OLDER snapshot for the same uuid.
    let (pkg_dir, announce) = build_fixture_package(
        tmp.path(),
        "frame-uuid-3",
        "L_0003.fits",
        "ORIGINAL_NAME",
        "2020-01-01T00:00:00.000Z",
    );
    let out = ingest_package(
        IngestConn::Borrowed(&conn),
        &incoming,
        &pkg_dir,
        &announce,
        PEER_HEX,
        &announce.package_id.0,
        None,
        None,
    )
    .unwrap();

    // Not overwritten: still one frame, object unchanged, receipt Duplicate.
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM frames"),
        1,
        "no new frame inserted"
    );
    let object: String = conn
        .query_row(
            "SELECT object FROM frames WHERE uuid='frame-uuid-3'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(object, "EDITED_ON_PRIMARY", "primary edit preserved");
    assert_eq!(out.skipped_older, 1, "counted as skipped_older");
    assert!(matches!(out.receipts[0].outcome, ReceiptOutcome::Duplicate));
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM sync_history WHERE outcome='skipped_older'"
        ),
        1,
        "history notes skipped_older"
    );
}

// ── ack_replay: full receiver over LoopbackTransport ────────────────────────

/// Wait for the next `AckReceived` on `events` matching `package_id`.
async fn wait_for_ack(
    events: &mut Receiver<TransportEvent>,
    package_id: &str,
    timeout: Duration,
) -> Vec<FrameReceipt> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let ev = tokio::time::timeout(remaining, events.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for ack of {package_id}"))
            .expect("sender event stream closed");
        if let TransportEvent::AckReceived {
            package_id: id,
            receipts,
            ..
        } = ev
        {
            if id.0 == package_id {
                return receipts;
            }
        }
    }
}

#[tokio::test]
async fn ack_replay_from_receipt_log() {
    let tmp = TempDir::new().unwrap();
    let catalog_path = tmp.path().join("catalog.db");
    // Initialise the catalog schema (and hold a connection open for assertions).
    let assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();

    let sync_dir = tmp.path().join("sync");
    let incoming = sync_dir.join("incoming");
    let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());

    // Loopback network: sender + receiver endpoints.
    let net = LoopbackNetwork::new();
    let sender: Arc<LoopbackTransport> = Arc::new(net.endpoint());
    let receiver_ep: Arc<LoopbackTransport> = Arc::new(net.endpoint());
    let receiver_node: NodeId = receiver_ep.node_id();

    // Bring the sender online and take its ack stream.
    sender.start().await.unwrap();
    let mut sender_events = sender.events().await;

    // Spawn the real receiver over its endpoint.
    let (_info, _handle) = SyncReceiver::spawn(
        Arc::clone(&store),
        sync_dir.clone(),
        fixed_resolver(incoming.clone()),
        super::allow_all_peers(),
        Default::default(), // no project announce gate in this test
        Arc::new(crate::sync::InboundControl::new()),
        Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
        Arc::new(NullEmitter),
    )
    .await
    .unwrap();

    // Build + serve a fixture package.
    let (pkg_dir, announce) = build_fixture_package(
        tmp.path(),
        "frame-uuid-4",
        "L_0004.fits",
        "NGC7000",
        "2026-01-16T10:00:00.000Z",
    );
    sender.serve(&announce, &pkg_dir, None).await.unwrap();

    // First delivery: announce → receiver fetches, ingests, acks.
    sender
        .announce(receiver_node, &announce, "", "", &[], PackageLayout::Batch)
        .await
        .unwrap();
    let receipts1 = wait_for_ack(
        &mut sender_events,
        &announce.package_id.0,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(receipts1.len(), 1);
    assert!(matches!(receipts1[0].outcome, ReceiptOutcome::Ingested));

    // The first ingest landed exactly one file/frame and wrote one history +
    // one receipt row. Snapshot the inbound row's generation + Done state so the
    // pure-replay assertions below can prove neither changed.
    let (gen_after_first, state_after_first) = {
        let c = assert_db.conn();
        assert_eq!(
            count(&c, "SELECT COUNT(*) FROM files"),
            1,
            "first delivery ingested one file"
        );
        assert_eq!(
            count(&c, "SELECT COUNT(*) FROM sync_history"),
            1,
            "one history row after first"
        );
        assert_eq!(
            count(&c, "SELECT COUNT(*) FROM sync_receipts"),
            1,
            "one receipt row after first"
        );
        let generation = count(&c, "SELECT generation FROM sync_inbound");
        let state: String = c
            .query_row("SELECT state FROM sync_inbound", [], |r| r.get(0))
            .unwrap();
        (generation, state)
    };
    assert_eq!(
        state_after_first, "done",
        "the first delivery left the row Done"
    );

    // Second delivery of the SAME announce (same package_id): the receiver must
    // re-ack from the receipt log WITHOUT re-fetching or re-ingesting.
    sender
        .announce(receiver_node, &announce, "", "", &[], PackageLayout::Batch)
        .await
        .unwrap();
    let receipts2 = wait_for_ack(
        &mut sender_events,
        &announce.package_id.0,
        Duration::from_secs(5),
    )
    .await;

    // Identical receipts, replayed straight from the log.
    assert_eq!(receipts2.len(), 1);
    assert_eq!(receipts2[0].frame_uuid, receipts1[0].frame_uuid);
    assert_eq!(receipts2[0].xxh3, receipts1[0].xxh3);

    // No re-ingest: file/frame count unchanged AND the replay wrote no new
    // history or receipt rows (replay short-circuits before ingest). Crucially,
    // the pure-replay guard (Transfers smoke №8, item 4) skips the upsert entirely
    // on a same-wire re-announce of an already-terminal row, so the generation is
    // UNCHANGED and the state stays Done — pre-fix the upsert silently reset the
    // row to `announced` (generation+1) and the post-upsert guard re-stamped it.
    {
        let c = assert_db.conn();
        assert_eq!(
            count(&c, "SELECT COUNT(*) FROM files"),
            1,
            "replay did not re-ingest a file"
        );
        assert_eq!(
            count(&c, "SELECT COUNT(*) FROM sync_history"),
            1,
            "replay wrote no history row"
        );
        assert_eq!(
            count(&c, "SELECT COUNT(*) FROM sync_receipts"),
            1,
            "replay wrote no receipt row"
        );
        assert_eq!(
            count(&c, "SELECT generation FROM sync_inbound"),
            gen_after_first,
            "the pure-replay guard never bumps generation (no upsert reset)"
        );
        let state_after_second: String = c
            .query_row("SELECT state FROM sync_inbound", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            state_after_second, "done",
            "the row stays Done across the pure replay"
        );
    }
}

/// Transfers smoke №8 (item 4) regression: a duplicate announce arriving on the
/// SAME wire id AFTER the transfer is Done — with the re-ack then FAILING — must
/// leave the inbound row untouched (Done, generation unchanged), write no new
/// history or receipts, and never strand the row at `announced`. Pre-fix, the
/// upsert reset the Done row to `announced` and the post-upsert replay guard's `?`
/// aborted on the failed re-ack before re-stamping Done, freezing the row
/// `announced` forever (the owner's live stuck row id=1). A THIRD announce (ack
/// now allowed) replays cleanly.
#[tokio::test]
async fn duplicate_announce_after_done_survives_failed_reack() {
    let tmp = TempDir::new().unwrap();
    let catalog_path = tmp.path().join("catalog.db");
    let assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();

    let sync_dir = tmp.path().join("sync");
    let incoming = sync_dir.join("incoming");
    let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());

    let net = LoopbackNetwork::new();
    let sender: Arc<LoopbackTransport> = Arc::new(net.endpoint());
    let receiver_ep: Arc<LoopbackTransport> = Arc::new(net.endpoint());
    let receiver_node: NodeId = receiver_ep.node_id();

    sender.start().await.unwrap();
    let mut sender_events = sender.events().await;

    let (_info, _handle) = SyncReceiver::spawn(
        Arc::clone(&store),
        sync_dir.clone(),
        fixed_resolver(incoming.clone()),
        super::allow_all_peers(),
        Default::default(),
        Arc::new(crate::sync::InboundControl::new()),
        Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
        Arc::new(NullEmitter),
    )
    .await
    .unwrap();

    let (pkg_dir, announce) = build_fixture_package(
        tmp.path(),
        "frame-uuid-dup",
        "L_0009.fits",
        "NGC6888",
        "2026-01-17T10:00:00.000Z",
    );
    sender.serve(&announce, &pkg_dir, None).await.unwrap();

    // First delivery: fetch → ingest → ack (Done). Snapshot generation + state.
    sender
        .announce(receiver_node, &announce, "", "", &[], PackageLayout::Batch)
        .await
        .unwrap();
    let receipts1 = wait_for_ack(
        &mut sender_events,
        &announce.package_id.0,
        Duration::from_secs(5),
    )
    .await;
    assert!(matches!(receipts1[0].outcome, ReceiptOutcome::Ingested));
    let gen_after_first = {
        let c = assert_db.conn();
        assert_eq!(
            count(&c, "SELECT COUNT(*) FROM sync_history"),
            1,
            "one history row after first"
        );
        assert_eq!(
            count(&c, "SELECT COUNT(*) FROM sync_receipts"),
            1,
            "one receipt row after first"
        );
        let state: String = c
            .query_row("SELECT state FROM sync_inbound", [], |r| r.get(0))
            .unwrap();
        assert_eq!(state, "done", "first delivery leaves the row Done");
        count(&c, "SELECT generation FROM sync_inbound")
    };

    // Arm the one-shot ack fault, then re-announce the SAME wire id. The pure-replay
    // guard fires; its re-ack FAILS (fault) but is non-fatal, and the terminal stamp
    // ran first, so the row must stay Done. Poll the journal for the `replayed` entry
    // (no ack event arrives — the fault ate it) to know the replay finished.
    receiver_ep.set_fault(FaultPlan {
        fail_ack_once: true,
        ..Default::default()
    });
    sender
        .announce(receiver_node, &announce, "", "", &[], PackageLayout::Batch)
        .await
        .unwrap();
    {
        let mut ok = false;
        for _ in 0..400 {
            if count(
                &assert_db.conn(),
                "SELECT COUNT(*) FROM sync_events WHERE kind='replayed'",
            ) == 1
            {
                ok = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(ok, "the failed re-ack still journaled the replay");
        let c = assert_db.conn();
        let state: String = c
            .query_row("SELECT state FROM sync_inbound", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            state, "done",
            "a FAILED re-ack must never strand the row at announced — it stays Done"
        );
        assert_eq!(
            count(&c, "SELECT generation FROM sync_inbound"),
            gen_after_first,
            "the pure-replay guard skips the upsert, so generation is unchanged"
        );
        assert_eq!(
            count(&c, "SELECT COUNT(*) FROM sync_history"),
            1,
            "the failed re-ack wrote no new history"
        );
        assert_eq!(
            count(&c, "SELECT COUNT(*) FROM sync_receipts"),
            1,
            "the failed re-ack wrote no new receipt"
        );
    }

    // Third announce (fault disarmed): the replay ack now succeeds and the row is
    // still Done, still generation-stable.
    sender
        .announce(receiver_node, &announce, "", "", &[], PackageLayout::Batch)
        .await
        .unwrap();
    let receipts3 = wait_for_ack(
        &mut sender_events,
        &announce.package_id.0,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(
        receipts3[0].frame_uuid, receipts1[0].frame_uuid,
        "the third announce replays the same receipts"
    );
    {
        let c = assert_db.conn();
        let state: String = c
            .query_row("SELECT state FROM sync_inbound", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            state, "done",
            "the row stays Done after the successful replay"
        );
        assert_eq!(
            count(&c, "SELECT generation FROM sync_inbound"),
            gen_after_first,
            "generation still unchanged"
        );
        assert_eq!(
            count(&c, "SELECT COUNT(*) FROM sync_history"),
            1,
            "still one history row"
        );
    }
}

// ── Fix-review regression tests (reject-aware ack + full-hash secondary dedupe) ──

/// Required test #2: the ack-replay guard (`count_satisfied_receipts`) must
/// exclude `Rejected` receipts from its "fully receipted" count — a package
/// with a pending rejection is NOT short-circuited (it needs a real
/// redelivery), but once every receipt is non-rejected, the guard IS satisfied.
#[test]
fn replay_guard_excludes_rejected_receipts() {
    let tmp = TempDir::new().unwrap();
    let store = CatalogSyncStore::open(tmp.path().join("catalog.db")).unwrap();
    let package_id = "pkg-replay-guard";
    let frame_count = 2u32;

    // Two frames: one Ingested, one Rejected — NOT fully satisfied.
    {
        let conn = store.lock_conn();
        insert_receipt(
            &conn,
            package_id,
            &FrameReceipt {
                frame_uuid: "f1".into(),
                xxh3: "h1".into(),
                outcome: ReceiptOutcome::Ingested,
            },
            "2026-01-01T00:00:00.000Z",
        )
        .unwrap();
        insert_receipt(
            &conn,
            package_id,
            &FrameReceipt {
                frame_uuid: "f2".into(),
                xxh3: "h2".into(),
                outcome: ReceiptOutcome::Rejected("xxh3 mismatch".into()),
            },
            "2026-01-01T00:00:00.000Z",
        )
        .unwrap();
    }
    let total = store
        .count_receipts(&PackageId(package_id.to_string()))
        .unwrap();
    assert_eq!(total, 2, "both receipts recorded");
    let satisfied = {
        let conn = store.lock_conn();
        count_satisfied_receipts(&conn, package_id).unwrap()
    };
    assert_eq!(
        satisfied, 1,
        "a Rejected receipt must not count as satisfied"
    );
    assert_ne!(
        satisfied, frame_count,
        "guard must NOT short-circuit while a rejection is pending"
    );

    // Upgrade f2's receipt to Ingested (simulating a successful redelivery) —
    // the package is now fully satisfied.
    {
        let conn = store.lock_conn();
        insert_receipt(
            &conn,
            package_id,
            &FrameReceipt {
                frame_uuid: "f2".into(),
                xxh3: "h2".into(),
                outcome: ReceiptOutcome::Ingested,
            },
            "2026-01-01T00:01:00.000Z",
        )
        .unwrap();
    }
    let satisfied2 = {
        let conn = store.lock_conn();
        count_satisfied_receipts(&conn, package_id).unwrap()
    };
    assert_eq!(
        satisfied2, frame_count,
        "once every receipt is non-rejected, the guard IS satisfied"
    );
}

/// Required test #3: two distinct 4MB payloads whose 3-position *sampling*
/// hash (`duplicates::compute_xxhash`) collides because they are byte-identical
/// in all three sampled windows, differing only at an offset in the un-sampled
/// gap between window 1 and window 2. Window math (verified against
/// `duplicates::compute_xxhash`'s source for `SIZE = 4 MiB`):
/// window1 = `[0, 524288)`, window2 = `[1835008, 2359296)`, window3 =
/// `[3670016, 4194304)` — `DIVERGE_OFFSET = 1_000_000` sits in the gap
/// `[524288, 1835008)`, outside all three. Before the fix-review's fix, this
/// second (distinct-uuid, distinct-full-hash) file was wrongly skipped as a
/// content-hash `Duplicate`; after the fix (secondary dedupe compares the
/// manifest's full xxh3 against `sync_receipts`, never the sampling hash) it
/// must be genuinely ingested.
#[test]
fn sampling_collision_is_not_treated_as_duplicate() {
    const SIZE: usize = 4 * 1024 * 1024;
    const DIVERGE_OFFSET: usize = 1_000_000;

    let buf_a = vec![0u8; SIZE];
    let mut buf_b = buf_a.clone();
    buf_b[DIVERGE_OFFSET] = 0xFF;

    let tmp = TempDir::new().unwrap();
    let incoming = tmp.path().join("incoming");
    let conn = catalog_conn();

    let (pkg_a, announce_a) = build_raw_package(tmp.path(), "frame-collide-a", "A.bin", &buf_a);
    let (pkg_b, announce_b) = build_raw_package(tmp.path(), "frame-collide-b", "B.bin", &buf_b);

    // Sanity-check the premise itself: sampling hash collides, full hash
    // differs. If a future change to `compute_xxhash`'s window math breaks
    // this, the test must fail here with a clear message, not silently pass
    // for the wrong reason.
    let sampling_a = crate::duplicates::compute_xxhash(&pkg_a.join("A.bin")).unwrap();
    let sampling_b = crate::duplicates::compute_xxhash(&pkg_b.join("B.bin")).unwrap();
    assert_eq!(
        sampling_a, sampling_b,
        "test premise: sampling hashes must collide"
    );
    assert_ne!(
        announce_a.root_hash, announce_b.root_hash,
        "sanity: packages are not identical"
    );
    assert_ne!(
        package::xxh3_full_file(&pkg_a.join("A.bin")).unwrap(),
        package::xxh3_full_file(&pkg_b.join("B.bin")).unwrap(),
        "test premise: full content hash must differ"
    );

    let out_a = ingest_package(
        IngestConn::Borrowed(&conn),
        &incoming,
        &pkg_a,
        &announce_a,
        PEER_HEX,
        &announce_a.package_id.0,
        None,
        None,
    )
    .unwrap();
    assert_eq!(out_a.ingested, 1);

    let out_b = ingest_package(
        IngestConn::Borrowed(&conn),
        &incoming,
        &pkg_b,
        &announce_b,
        PEER_HEX,
        &announce_b.package_id.0,
        None,
        None,
    )
    .unwrap();
    assert_eq!(
        out_b.ingested, 1,
        "distinct content must ingest despite a sampling-hash collision with an already-ingested frame"
    );
    assert!(matches!(
        out_b.receipts[0].outcome,
        ReceiptOutcome::Ingested
    ));
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM files"),
        2,
        "both frames land as separate files"
    );
}

/// Content-dedup (step 3) must consult the LIVE catalog: a receipt only vouches
/// for content while the frame it ingested still exists in `frames`. Field bug
/// this pins: a user received a batch (receipts written `ingested`), later
/// deleted those files properly (disk + `DELETE FROM files` CASCADE → the
/// `frames` rows are gone too), then had them re-sent from another device. The
/// pre-handshake correctly *wanted* them and the bytes travelled, but step 3
/// found the OLD receipts and classified every frame `Duplicate`, discarding the
/// payload — once-received content banned forever on that machine. After the fix
/// (join `sync_receipts` to `frames` on `frame_uuid`), a receipt whose frame was
/// deleted no longer blocks the re-receive.
#[test]
fn content_reingest_allowed_after_catalog_delete() {
    let tmp = TempDir::new().unwrap();
    let incoming = tmp.path().join("incoming");
    let conn = catalog_conn();

    // The resend from another device: a NEW wire package with a fresh package_id
    // and a fresh frame_uuid.
    let (pkg_dir, announce) = build_fixture_package(
        tmp.path(),
        "frame-reingest-new",
        "L_0001.fits",
        "M31",
        "2026-01-16T10:00:00.000Z",
    );
    let full_hash = package::xxh3_full_file(&pkg_dir.join("L_0001.fits")).unwrap();

    // Seed a stale `ingested` receipt for an EARLIER package: same full-content
    // hash H, but its frame was deleted from the catalog (no `frames` row for
    // that uuid) — so the receipt is history, not live state.
    insert_receipt(
        &conn,
        "earlier-package-id",
        &FrameReceipt {
            frame_uuid: "deleted-frame-uuid".into(),
            xxh3: full_hash.clone(),
            outcome: ReceiptOutcome::Ingested,
        },
        "2026-01-01T00:00:00.000Z",
    )
    .unwrap();
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM frames WHERE uuid='deleted-frame-uuid'"
        ),
        0,
        "premise: the receipt's frame was deleted from the catalog"
    );

    let out = ingest_package(
        IngestConn::Borrowed(&conn),
        &incoming,
        &pkg_dir,
        &announce,
        PEER_HEX,
        &announce.package_id.0,
        None,
        None,
    )
    .unwrap();

    assert_eq!(
        out.ingested, 1,
        "deleted-then-resent content must re-ingest, not dedup against a stale receipt"
    );
    assert_eq!(out.duplicate, 0, "not a duplicate");
    assert!(matches!(out.receipts[0].outcome, ReceiptOutcome::Ingested));
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM files"),
        1,
        "catalog file row created"
    );
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM frames"),
        1,
        "catalog frame row created"
    );
    let landed_path: String = conn
        .query_row("SELECT path FROM files LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert!(
        Path::new(&landed_path).exists(),
        "payload landed on disk: {landed_path}"
    );
}

/// The complement pin (must pass before AND after the fix): while the receipt's
/// frame is STILL ALIVE in `frames`, content-dedup keeps blocking — a second
/// frame with the same content hash but a different uuid is a genuine
/// `Duplicate`, no write. This is also the guarantee that a black-holed frame
/// (still present in `frames`) keeps dedup-ing as present: the live-catalog join
/// preserves it automatically.
#[test]
fn content_dedup_still_blocks_while_frame_alive() {
    let tmp = TempDir::new().unwrap();
    let incoming = tmp.path().join("incoming");
    let conn = catalog_conn();

    // A new package (fresh wire id + fresh uuid) whose payload hashes to H.
    let (pkg_dir, announce) = build_fixture_package(
        tmp.path(),
        "frame-dup-new",
        "L_0002.fits",
        "M42",
        "2026-01-16T10:00:00.000Z",
    );
    let full_hash = package::xxh3_full_file(&pkg_dir.join("L_0002.fits")).unwrap();

    // A LIVE frame the receipt vouches for: files + frames rows present.
    conn.execute(
        "INSERT INTO files (path, filename, size, modified_at, format, created_at)
         VALUES ('/local/alive.fits', 'alive.fits', 100, '2026-01-15T00:00:00Z', 'FITS', '2026-01-15T00:00:00Z')",
        [],
    )
    .unwrap();
    let file_id: i64 = conn
        .query_row("SELECT id FROM files LIMIT 1", [], |r| r.get(0))
        .unwrap();
    conn.execute(
        "INSERT INTO frames (file_id, imagetyp, uuid, updated_at)
         VALUES (?1, 'Light', 'alive-frame-uuid', '2026-01-15T00:00:00.000Z')",
        rusqlite::params![file_id],
    )
    .unwrap();

    // The receipt: same full-content hash H, frame_uuid points at the live frame.
    insert_receipt(
        &conn,
        "earlier-package-id",
        &FrameReceipt {
            frame_uuid: "alive-frame-uuid".into(),
            xxh3: full_hash.clone(),
            outcome: ReceiptOutcome::Ingested,
        },
        "2026-01-01T00:00:00.000Z",
    )
    .unwrap();

    let out = ingest_package(
        IngestConn::Borrowed(&conn),
        &incoming,
        &pkg_dir,
        &announce,
        PEER_HEX,
        &announce.package_id.0,
        None,
        None,
    )
    .unwrap();

    assert_eq!(
        out.duplicate, 1,
        "same content while its frame is alive stays a Duplicate"
    );
    assert_eq!(out.ingested, 0, "no ingest");
    assert!(matches!(out.receipts[0].outcome, ReceiptOutcome::Duplicate));
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM frames"),
        1,
        "no new frame written"
    );
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM files"),
        1,
        "no new file written"
    );
}

/// Required test #1 (the e2e repair scenario): a two-frame package where one
/// payload is corrupted post-write. The receiver rejects that frame and
/// ingests its sibling; the sender must NOT confirm (any Rejected receipt
/// blocks confirmation) and keeps retrying. The test then repairs the source
/// file; the next redelivery reprocesses ONLY the previously-rejected frame
/// (the sibling's already-Ingested receipt is reused, not touched again) and
/// the sender finally confirms once every receipt is non-rejected.
#[tokio::test]
async fn transit_corruption_repaired_then_redelivery_confirms() {
    let tmp = TempDir::new().unwrap();
    let net = LoopbackNetwork::new();

    // Build the two-frame package, then corrupt frame B's SERVED payload
    // (pkg_dir is what the sender re-serves on every retry, so repairing it
    // later is what a redelivery actually picks up).
    let (pkg_dir, [uuid_a, uuid_b], path_b_in_pkg) = build_two_frame_package(tmp.path());
    corrupt_at(&path_b_in_pkg, 100);

    // Receiver: a real SyncReceiver over a catalog DB.
    let catalog_path = tmp.path().join("catalog.db");
    let assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
    let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());
    let incoming = tmp.path().join("incoming");

    let receiver_ep = Arc::new(net.endpoint());
    let receiver_node = receiver_ep.node_id();
    let (_info, _handle) = SyncReceiver::spawn(
        Arc::clone(&store),
        tmp.path().join("staging_root"),
        fixed_resolver(incoming),
        super::allow_all_peers(),
        Default::default(), // no project announce gate in this test
        Arc::new(crate::sync::InboundControl::new()),
        Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
        Arc::new(NullEmitter),
    )
    .await
    .unwrap();

    // Sender: a real SyncEngine with a short ack-timeout so retries happen
    // quickly and deterministically within the test's wait budget.
    let sync_store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let engine = SyncEngine::spawn_with_config(
        sync_store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_node,
        SyncConfig {
            ack_timeout: Duration::from_millis(60),
        },
    );

    let id = engine
        .enqueue_package(&pkg_dir, None, Vec::new(), PackageLayout::Batch)
        .await
        .unwrap();

    // First delivery: frame A ingests, frame B is rejected (corrupted) — the
    // sender must NOT confirm. Wait for the good frame to land, then assert the
    // package stays non-Confirmed across at least one retry cycle.
    wait_until(
        || count(&assert_db.conn(), "SELECT COUNT(*) FROM files") == 1,
        Duration::from_secs(5),
    )
    .await;
    wait_until(|| attempts_of(&sync_store, id) >= 1, Duration::from_secs(5)).await;
    assert_ne!(
        state_of(&sync_store, id),
        Some(super::OutboundState::Confirmed),
        "a package with a rejected frame must not be confirmed"
    );
    assert_eq!(
        count(&assert_db.conn(), "SELECT COUNT(*) FROM files"),
        1,
        "only the good frame ingested so far"
    );

    // Repair the source: restore frame B's original (uncorrupted) bytes at the
    // path the sender re-serves on every retry.
    write_fits_val(&path_b_in_pkg, 1.0);

    // Redelivery: the engine keeps re-announcing (same package_id) until the
    // receiver acks with zero rejections, at which point the sender confirms.
    wait_until(
        || state_of(&sync_store, id) == Some(super::OutboundState::Confirmed),
        Duration::from_secs(10),
    )
    .await;
    assert_eq!(
        state_of(&sync_store, id),
        Some(super::OutboundState::Confirmed)
    );

    // Single catalog row per frame — the good frame was never reprocessed, and
    // the repaired frame was ingested exactly once on the redelivery attempt
    // that finally verified.
    let conn = assert_db.conn();
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM files"),
        2,
        "one files row per frame, no duplicates"
    );
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM frames"),
        2,
        "one frames row per frame, no duplicates"
    );

    // Receipt for the repaired frame was upserted from Rejected to Ingested.
    let b_outcome: String = conn
        .query_row(
            "SELECT outcome FROM sync_receipts WHERE frame_uuid = ?1",
            rusqlite::params![uuid_b],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        b_outcome, "ingested",
        "the repaired frame's receipt was upserted to ingested"
    );

    // History shows the reject-then-ingest trail for the repaired frame, and
    // exactly one ingested row for the never-touched-again good frame.
    let b_history_outcomes: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT outcome FROM sync_history WHERE frame_uuid = ?1 ORDER BY id")
            .unwrap();
        stmt.query_map(rusqlite::params![uuid_b], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<String>>>()
            .unwrap()
    };
    assert!(
        b_history_outcomes.iter().any(|o| o.starts_with("rejected")),
        "history must show the initial rejection: {b_history_outcomes:?}"
    );
    assert!(
        b_history_outcomes.iter().any(|o| o == "ingested"),
        "history must show the eventual ingest: {b_history_outcomes:?}"
    );

    let a_history_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sync_history WHERE frame_uuid = ?1",
            rusqlite::params![uuid_a],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        a_history_count, 1,
        "the good frame's history is not touched again on redelivery"
    );

    engine.shutdown().await;
}

fn state_of(store: &StandaloneSyncStore, id: i64) -> Option<super::OutboundState> {
    store.get_outbound(id).unwrap().map(|r| r.state)
}

fn attempts_of(store: &StandaloneSyncStore, id: i64) -> u32 {
    store
        .get_outbound(id)
        .unwrap()
        .map(|r| r.attempts)
        .unwrap_or(0)
}

/// A resolver that always lands under a single fixed `root` — the pre-task-5
/// behaviour, for the receiver tests that only assert catalog rows/receipts.
fn fixed_resolver(root: PathBuf) -> IncomingResolver {
    Arc::new(move || root.clone())
}

/// Build a one-frame fixture package with a distinct pixel `val` — like
/// [`build_fixture_package`] but with genuinely different content per call so two
/// packages both ingest (all-zero payloads would collide on the full-content
/// secondary dedup and the second would land nothing). Returns `(pkg_dir, announce)`.
fn build_fixture_package_val(
    root: &Path,
    frame_uuid: &str,
    filename: &str,
    object: &str,
    val: f32,
) -> (PathBuf, PackageAnnounce) {
    let src_dir = root.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let src = src_dir.join(filename);
    write_fits_val(&src, val);

    let byte_size = std::fs::metadata(&src).unwrap().len();
    let xxh3 = package::xxh3_full_file(&src).unwrap();
    let frame = fixture_frame(frame_uuid, object, "2026-01-16T10:00:00.000Z");
    let record = ManifestRecord {
        v: MANIFEST_VERSION,
        frame_uuid: frame_uuid.to_string(),
        origin_catalog_uuid: "catalog-uuid".to_string(),
        origin_device: ORIGIN_DEVICE.to_string(),
        payload_kind: PayloadKind::RawFrame,
        rel_path: filename.to_string(),
        byte_size,
        xxh3,
        frame_meta: serde_json::to_value(&frame).unwrap(),
        analysis: None,
        app_version: "test".to_string(),
        project: None,
    };
    let pkg_dir = root.join(format!("pkg-{frame_uuid}"));
    let announce = package::write_package(&pkg_dir, vec![(src, record)]).unwrap();
    (pkg_dir, announce)
}

/// Count regular files anywhere under `dir` (recursive). Used to prove a package
/// landed under one resolver target and not the other.
fn count_files(dir: &Path) -> usize {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .count()
}

/// The landing root is resolved LIVE, once per package — swapping the resolver's
/// target between two announces makes the second package land under the new root
/// with no receiver restart. Pins the "per-package resolution" contract (task 5).
#[tokio::test]
async fn landing_root_is_resolved_live_per_package() {
    let tmp = TempDir::new().unwrap();
    let catalog_path = tmp.path().join("catalog.db");
    // Initialise the catalog schema (idempotent) so the receiver can ingest.
    let _assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
    let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());

    let dir_a = tmp.path().join("root_a");
    let dir_b = tmp.path().join("root_b");
    let staging_root = tmp.path().join("staging_root");

    // A resolver whose target is a swappable `Arc<Mutex<PathBuf>>` (the exact
    // mechanic the host uses via a live DB lookup — here the swap is explicit).
    let target = Arc::new(std::sync::Mutex::new(dir_a.clone()));
    let resolver: IncomingResolver = {
        let t = Arc::clone(&target);
        Arc::new(move || t.lock().unwrap().clone())
    };

    let net = LoopbackNetwork::new();
    let sender: Arc<LoopbackTransport> = Arc::new(net.endpoint());
    let receiver_ep: Arc<LoopbackTransport> = Arc::new(net.endpoint());
    let receiver_node: NodeId = receiver_ep.node_id();

    sender.start().await.unwrap();
    let mut sender_events = sender.events().await;

    let (_info, _handle) = SyncReceiver::spawn(
        Arc::clone(&store),
        staging_root.clone(),
        Arc::clone(&resolver),
        super::allow_all_peers(),
        Default::default(), // no project announce gate in this test
        Arc::new(crate::sync::InboundControl::new()),
        Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
        Arc::new(NullEmitter),
    )
    .await
    .unwrap();

    // Package 1 → resolver returns dir_a → lands under dir_a. Distinct pixel
    // content per package so both genuinely ingest (never a content dup).
    let (pkg1, announce1) =
        build_fixture_package_val(tmp.path(), "frame-live-a", "L_live_a.fits", "M42", 0.0);
    sender.serve(&announce1, &pkg1, None).await.unwrap();
    sender
        .announce(receiver_node, &announce1, "", "", &[], PackageLayout::Batch)
        .await
        .unwrap();
    let r1 = wait_for_ack(
        &mut sender_events,
        &announce1.package_id.0,
        Duration::from_secs(5),
    )
    .await;
    assert!(matches!(r1[0].outcome, ReceiptOutcome::Ingested));
    assert_eq!(
        count_files(&dir_a),
        1,
        "package 1 landed under the first resolver target"
    );
    assert_eq!(
        count_files(&dir_b),
        0,
        "package 1 did not land under the second target"
    );

    // Swap the resolver's target: package 2 must land under dir_b, no restart.
    *target.lock().unwrap() = dir_b.clone();

    let (pkg2, announce2) =
        build_fixture_package_val(tmp.path(), "frame-live-b", "L_live_b.fits", "NGC7000", 1.0);
    sender.serve(&announce2, &pkg2, None).await.unwrap();
    sender
        .announce(receiver_node, &announce2, "", "", &[], PackageLayout::Batch)
        .await
        .unwrap();
    let r2 = wait_for_ack(
        &mut sender_events,
        &announce2.package_id.0,
        Duration::from_secs(5),
    )
    .await;
    assert!(matches!(r2[0].outcome, ReceiptOutcome::Ingested));
    assert_eq!(
        count_files(&dir_b),
        1,
        "package 2 landed under the NEW resolver target (live per-package)"
    );
    assert_eq!(
        count_files(&dir_a),
        1,
        "package 2 did not re-land under the first target"
    );
}

/// Poll a sync predicate every 10ms until true, panicking after `timeout`.
/// Local copy of `engine_tests::wait_until` (private to its own module, so this
/// file needs its own).
async fn wait_until<F: FnMut() -> bool>(mut pred: F, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if pred() {
            return;
        }
        if Instant::now() >= deadline {
            panic!("wait_until timed out after {timeout:?}");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

// ── H1: receiver-side peer authorization ─────────────────────────────────────

/// H1 (deny): an announce from a peer NOT on the receiver's allow-list is
/// silently dropped — nothing is fetched, ingested, or landed. Without the gate
/// the fixture package would ingest one file + one history row.
#[tokio::test]
async fn receiver_drops_announce_from_unauthorized_peer() {
    let tmp = TempDir::new().unwrap();
    let catalog_path = tmp.path().join("catalog.db");
    let assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
    let sync_dir = tmp.path().join("sync");
    let incoming = sync_dir.join("incoming");
    let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());

    let net = LoopbackNetwork::new();
    let sender: Arc<LoopbackTransport> = Arc::new(net.endpoint());
    let receiver_ep: Arc<LoopbackTransport> = Arc::new(net.endpoint());
    let receiver_node: NodeId = receiver_ep.node_id();
    sender.start().await.unwrap();

    // Allow-list contains a DIFFERENT node, never the actual sender.
    let allowed_other: NodeId = [9u8; 32];
    let authorizer: super::PeerAuthorizer = Arc::new(move |id| *id == allowed_other);

    let (_info, _handle) = SyncReceiver::spawn(
        Arc::clone(&store),
        sync_dir.clone(),
        fixed_resolver(incoming.clone()),
        authorizer,
        Default::default(), // no project announce gate in this test
        Arc::new(crate::sync::InboundControl::new()),
        Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
        Arc::new(NullEmitter),
    )
    .await
    .unwrap();

    let (pkg_dir, announce) = build_fixture_package(
        tmp.path(),
        "frame-uuid-deny",
        "L_deny.fits",
        "M42",
        "2026-01-16T10:00:00.000Z",
    );
    sender.serve(&announce, &pkg_dir, None).await.unwrap();
    sender
        .announce(receiver_node, &announce, "", "", &[], PackageLayout::Batch)
        .await
        .unwrap();

    // Give the receiver ample time to (wrongly) ingest, then assert it did not.
    tokio::time::sleep(Duration::from_millis(400)).await;
    let c = assert_db.conn();
    assert_eq!(
        count(&c, "SELECT COUNT(*) FROM files"),
        0,
        "an unauthorized peer's announce must ingest nothing"
    );
    assert_eq!(
        count(&c, "SELECT COUNT(*) FROM sync_history"),
        0,
        "a dropped announce writes no history"
    );
}

/// H1 (allow): an announce from a peer ON the allow-list ingests normally — the
/// gate rejects only unauthorized senders, never authorized ones.
#[tokio::test]
async fn receiver_ingests_from_authorized_peer() {
    let tmp = TempDir::new().unwrap();
    let catalog_path = tmp.path().join("catalog.db");
    let assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
    let sync_dir = tmp.path().join("sync");
    let incoming = sync_dir.join("incoming");
    let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());

    let net = LoopbackNetwork::new();
    let sender: Arc<LoopbackTransport> = Arc::new(net.endpoint());
    let receiver_ep: Arc<LoopbackTransport> = Arc::new(net.endpoint());
    let receiver_node: NodeId = receiver_ep.node_id();
    sender.start().await.unwrap();
    let mut sender_events = sender.events().await;

    // Allow-list is exactly the real sender.
    let sender_node: NodeId = sender.node_id();
    let authorizer: super::PeerAuthorizer = Arc::new(move |id| *id == sender_node);

    let (_info, _handle) = SyncReceiver::spawn(
        Arc::clone(&store),
        sync_dir.clone(),
        fixed_resolver(incoming.clone()),
        authorizer,
        Default::default(), // no project announce gate in this test
        Arc::new(crate::sync::InboundControl::new()),
        Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
        Arc::new(NullEmitter),
    )
    .await
    .unwrap();

    let (pkg_dir, announce) = build_fixture_package(
        tmp.path(),
        "frame-uuid-allow",
        "L_allow.fits",
        "M42",
        "2026-01-16T10:00:00.000Z",
    );
    sender.serve(&announce, &pkg_dir, None).await.unwrap();
    sender
        .announce(receiver_node, &announce, "", "", &[], PackageLayout::Batch)
        .await
        .unwrap();

    let receipts = wait_for_ack(
        &mut sender_events,
        &announce.package_id.0,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(receipts.len(), 1);
    assert!(matches!(receipts[0].outcome, ReceiptOutcome::Ingested));
    let c = assert_db.conn();
    assert_eq!(
        count(&c, "SELECT COUNT(*) FROM files"),
        1,
        "an authorized peer's announce ingests normally"
    );
}

// ── Project exchange (slice 4, task 5): full receiver over LoopbackTransport ──

/// Captures emitted events so the project e2e can assert `sync-finished` carried
/// the project id.
#[derive(Default)]
struct EventRecorder {
    events: std::sync::Mutex<Vec<(String, serde_json::Value)>>,
}
impl crate::events::ProgressEmitter for EventRecorder {
    fn emit_json(&self, name: &str, payload: serde_json::Value) {
        self.events
            .lock()
            .unwrap()
            .push((name.to_string(), payload));
    }
}

/// A cached `collab_projects` row with the given slug (dummy target/snapshot).
fn e2e_project_row(project_id: &str, slug: &str) -> crate::db::collab::CollabProjectRow {
    crate::db::collab::CollabProjectRow {
        project_id: project_id.to_string(),
        slug: slug.to_string(),
        title: "E2E".to_string(),
        data_role: "contribute".to_string(),
        is_coordinator: false,
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
        members_json: "[]".to_string(),
        thresholds_version: None,
        thresholds_rules_json: None,
        // local preference — ignored on write
        auto_replicate: true,
        fetched_at: String::new(),
    }
}

/// A hub-anchored `project_packages` row for the given package + manifest anchor.
fn e2e_package_row(
    project_id: &str,
    package_id: &str,
    publisher: &str,
    anchor: &str,
) -> crate::db::collab_exchange::PackageRow {
    crate::db::collab_exchange::PackageRow {
        package_id: package_id.to_string(),
        project_id: project_id.to_string(),
        announcement_id: format!("ann-{package_id}"),
        publisher_display: publisher.to_string(),
        own: false,
        root_hash: "rh".to_string(),
        byte_size: 0,
        frame_count: 1,
        manifest_xxh3: Some(anchor.to_string()),
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

/// Build a stamped one-frame PROJECT package under `root`; returns
/// `(pkg_dir, announce, manifest_anchor)`. The record carries a `ProjectStamp`
/// for `(project_id, hub_package_id)` (so the receiver's cross-check passes); the
/// anchor is the full-content xxh3 of the written `manifest.ndjson` — exactly what
/// the hub records as `manifest_xxh3`.
fn build_stamped_project_package(
    root: &Path,
    frame_uuid: &str,
    filename: &str,
    project_id: &str,
    hub_package_id: &str,
) -> (PathBuf, PackageAnnounce, String) {
    let src_dir = root.join("psrc");
    std::fs::create_dir_all(&src_dir).unwrap();
    let src = src_dir.join(filename);
    write_fits_val(&src, 0.5);
    let byte_size = std::fs::metadata(&src).unwrap().len();
    let xxh3 = package::xxh3_full_file(&src).unwrap();
    let frame = fixture_frame(frame_uuid, "M42", "2026-01-16T10:00:00.000Z");
    let record = ManifestRecord {
        v: MANIFEST_VERSION,
        frame_uuid: frame_uuid.to_string(),
        origin_catalog_uuid: "cat".to_string(),
        origin_device: ORIGIN_DEVICE.to_string(),
        payload_kind: PayloadKind::CalibratedLight,
        rel_path: filename.to_string(),
        byte_size,
        xxh3,
        frame_meta: serde_json::to_value(&frame).unwrap(),
        analysis: None,
        app_version: "test".to_string(),
        project: Some(crate::package::ProjectStamp {
            project_id: project_id.to_string(),
            package_id: hub_package_id.to_string(),
            thresholds_version: None,
            cal_engine_version: None,
        }),
    };
    let pkg_dir = root.join(format!("ppkg-{frame_uuid}"));
    let announce = package::write_package(&pkg_dir, vec![(src, record)]).unwrap();
    let anchor = package::xxh3_full_file(&pkg_dir.join(package::MANIFEST_FILENAME)).unwrap();
    (pkg_dir, announce, anchor)
}

/// Poll the recorder until a `sync-finished` event appears (the finished emit
/// races slightly behind the ack the sender awaits on).
async fn wait_for_finished(recorder: &EventRecorder) -> serde_json::Value {
    for _ in 0..300 {
        if let Some(v) = recorder
            .events
            .lock()
            .unwrap()
            .iter()
            .find(|(n, _)| n == "sync-finished")
            .map(|(_, v)| v.clone())
        {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no sync-finished emitted");
}

/// Loopback e2e (task 5 Step 3): node A push-seeds a stamped project package to
/// node B (B's catalog pre-seeded with the project + hub-anchored package row). B
/// lands the frame under `<collab-root>/<proj-slug>/<pub-slug>/`, writes a
/// contribution row (never `files`/`frames`), and acks — A sees the Ingested
/// receipts (receipts flowed), and B's `sync-finished` carries the project id.
#[tokio::test]
async fn project_package_lands_contributions_and_acks() {
    const PROJECT_ID: &str = "proj-e2e";
    const HUB_PACKAGE_ID: &str = "hub-pkg-e2e";
    const PROJECT_SLUG: &str = "orion-e2e";
    const PUBLISHER: &str = "Alice E2E";

    let tmp = TempDir::new().unwrap();
    let catalog_path = tmp.path().join("catalog.db");
    let assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();

    // Node A builds a stamped package; capture its manifest anchor.
    let (pkg_dir, announce, anchor) = build_stamped_project_package(
        tmp.path(),
        "pf-1",
        "L_0001.fits",
        PROJECT_ID,
        HUB_PACKAGE_ID,
    );

    // Seed B: project (slug) + hub-anchored package row + collaboration landing root.
    let landing = tmp.path().join("collab_landing");
    {
        let c = assert_db.conn();
        crate::db::collab::upsert_project(&c, &e2e_project_row(PROJECT_ID, PROJECT_SLUG)).unwrap();
        crate::db::collab_exchange::upsert_package(
            &c,
            &e2e_package_row(PROJECT_ID, HUB_PACKAGE_ID, PUBLISHER, &anchor),
        )
        .unwrap();
        c.execute(
            "INSERT INTO scan_roots (path, kind) VALUES (?1, 'collaboration')",
            [landing.to_string_lossy().to_string()],
        )
        .unwrap();
    }

    let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());
    let net = LoopbackNetwork::new();
    let sender: Arc<LoopbackTransport> = Arc::new(net.endpoint());
    let receiver_ep: Arc<LoopbackTransport> = Arc::new(net.endpoint());
    let receiver_node: NodeId = receiver_ep.node_id();

    sender.start().await.unwrap();
    let mut sender_events = sender.events().await;

    let recorder = Arc::new(EventRecorder::default());
    let hooks = super::receiver::ProjectReceiveHooks {
        gate: Some(Arc::new(|_from: &NodeId, pid: &str| pid == PROJECT_ID)),
        ..Default::default()
    };
    let (_info, _handle) = SyncReceiver::spawn(
        Arc::clone(&store),
        tmp.path().join("stage"),
        fixed_resolver(tmp.path().join("unused_incoming")),
        super::allow_all_peers(),
        hooks,
        Arc::new(crate::sync::InboundControl::new()),
        Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
        Arc::clone(&recorder) as Arc<dyn crate::events::ProgressEmitter>,
    )
    .await
    .unwrap();

    // A serves (full) + project-announces to B.
    sender.serve(&announce, &pkg_dir, None).await.unwrap();
    sender
        .announce_project(receiver_node, PROJECT_ID, HUB_PACKAGE_ID, &announce)
        .await
        .unwrap();

    // A receives the ack with an Ingested receipt ("receipts flowed", B confirmed).
    let receipts = wait_for_ack(
        &mut sender_events,
        &announce.package_id.0,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(receipts.len(), 1);
    assert!(matches!(receipts[0].outcome, ReceiptOutcome::Ingested));

    // B's finished event carries the project id.
    let finished = wait_for_finished(&recorder).await;
    assert_eq!(finished["projectId"], PROJECT_ID);
    assert_eq!(finished["outcome"], "ingested");
    assert_eq!(finished["okCount"], 1);

    // Contribution landed under <collab-root>/<proj-slug>/<pub-slug>/<rel>, never
    // in files/frames.
    let rows = {
        let c = assert_db.conn();
        assert_eq!(
            count(&c, "SELECT COUNT(*) FROM files"),
            0,
            "contributions never enter files"
        );
        assert_eq!(
            count(&c, "SELECT COUNT(*) FROM frames"),
            0,
            "contributions never enter frames"
        );
        crate::db::collab_exchange::contributions_for_package(&c, HUB_PACKAGE_ID).unwrap()
    };
    assert_eq!(rows.len(), 1, "one contribution landed");
    let landed = PathBuf::from(&rows[0].landed_path);
    let expected = landing
        .join(super::ingest::sanitize_slug(PROJECT_SLUG))
        .join(super::ingest::sanitize_slug(PUBLISHER))
        .join("L_0001.fits");
    assert_eq!(landed, expected, "hub-anchored landing layout");
    assert!(landed.exists(), "the payload is on disk");

    // The package is marked complete and its manifest retained (Д2 re-serve).
    let pkg = {
        let c = assert_db.conn();
        crate::db::collab_exchange::get_package(&c, HUB_PACKAGE_ID)
            .unwrap()
            .unwrap()
    };
    assert_eq!(pkg.local_status, "complete");
    assert!(
        pkg.manifest_ndjson.is_some(),
        "retained manifest bytes for re-serving"
    );
}

// ── W2 T2.1: per-frame connection locking (IngestConn) ──────────────────────

/// Frames per package for the two `IngestConn` tests: enough gaps between frames
/// for a competing thread to win the mutex at least twice, few enough to keep the
/// whole test well under a second of real work.
const CONN_TEST_FRAMES: usize = 16;

/// Build an `n`-frame fixture package whose payloads are real FITS files of
/// `dim`×`dim` f32 pixels (≈ `dim`²·4 bytes each) with a distinct fill value per
/// frame, so every frame has a genuinely distinct full-content hash and per-frame
/// ingest work (hash + copy + header extract + tx) has measurable duration.
fn build_multi_frame_package(root: &Path, n: usize, dim: usize) -> (PathBuf, PackageAnnounce) {
    let src_dir = root.join("src-multi");
    std::fs::create_dir_all(&src_dir).unwrap();

    let mut entries = Vec::with_capacity(n);
    for i in 0..n {
        let filename = format!("L_{i:04}.fits");
        let src = src_dir.join(&filename);
        write_fits_f32(&src, dim, dim, 1, &vec![i as f32 + 1.0; dim * dim], &[]).unwrap();
        let uuid = format!("frame-multi-{i}");
        let record = ManifestRecord {
            v: MANIFEST_VERSION,
            frame_uuid: uuid.clone(),
            origin_catalog_uuid: "catalog-uuid".to_string(),
            origin_device: ORIGIN_DEVICE.to_string(),
            payload_kind: PayloadKind::RawFrame,
            rel_path: filename,
            byte_size: std::fs::metadata(&src).unwrap().len(),
            xxh3: package::xxh3_full_file(&src).unwrap(),
            frame_meta: serde_json::to_value(fixture_frame(
                &uuid,
                "MULTI",
                "2026-01-16T10:00:00.000Z",
            ))
            .unwrap(),
            analysis: None,
            app_version: "test".to_string(),
            project: None,
        };
        entries.push((src, record));
    }

    let pkg_dir = root.join("pkg-multi");
    let announce = package::write_package(&pkg_dir, entries).unwrap();
    (pkg_dir, announce)
}

/// Run `run_ingest` while a competing thread hammers `store.lock_conn()`, and
/// return how many of the competitor's acquisitions observed a **partially
/// ingested** catalog (`0 < files < total_frames`).
///
/// That predicate is the whole point: an ingest that holds the guard for the
/// entire package commits every frame's transaction before any other thread can
/// read, so a competitor can only ever observe 0 (before) or `total_frames`
/// (after) — never a partial count. A non-zero result therefore *proves* the
/// guard was released between frames.
fn midpackage_observations(
    store: &CatalogSyncStore,
    total_frames: i64,
    run_ingest: impl FnOnce() + Send,
) -> usize {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    let done = AtomicBool::new(false);
    // Warm-up handshake: ingest must not start until the probe thread has
    // completed one full acquire-read-write cycle. Without it, a heavily loaded
    // machine (the full --workspace run keeps every core busy with sibling
    // tests) can finish the whole package before the probe thread is ever
    // scheduled — observed once as a 0-observation flake.
    let probe_warm = AtomicBool::new(false);
    let observations = AtomicUsize::new(0);

    std::thread::scope(|scope| {
        let probe = scope.spawn(|| {
            let mut i = 0u64;
            while !done.load(Ordering::Relaxed) {
                {
                    // One competing unit of work: lock, read, trivial write, drop.
                    let conn = store.lock_conn();
                    let files: i64 = conn
                        .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
                        .unwrap();
                    crate::db::set_setting(&conn, "test.conn_probe", &i.to_string()).unwrap();
                    if files > 0 && files < total_frames {
                        observations.fetch_add(1, Ordering::Relaxed);
                    }
                }
                probe_warm.store(true, Ordering::Relaxed);
                i += 1;
                std::thread::yield_now();
            }
        });

        while !probe_warm.load(Ordering::Relaxed) {
            std::thread::yield_now();
        }
        run_ingest();
        done.store(true, Ordering::Relaxed);
        probe.join().unwrap();
    });

    observations.load(Ordering::Relaxed)
}

/// W2 T2.1 — the bounded-wait pin. Ingesting a multi-frame package must release
/// the store connection between frames so a concurrent lane (another transfer's
/// fetch-sink state writes, or its own ingest) waits at most ONE frame, not the
/// whole multi-GB package.
#[test]
fn ingest_releases_conn_between_frames() {
    let tmp = TempDir::new().unwrap();
    let (pkg_dir, announce) = build_multi_frame_package(tmp.path(), CONN_TEST_FRAMES, 384);
    let frames = CONN_TEST_FRAMES as i64;

    // The pin: `IngestConn::Shared` locks per frame, so the competitor gets in
    // between frames and sees a partially-ingested catalog.
    let shared_catalog = tmp.path().join("catalog_shared.db");
    let _shared_db = crate::db::Database::new(shared_catalog.clone()).unwrap();
    let shared_store = CatalogSyncStore::open(&shared_catalog).unwrap();
    let shared_incoming = tmp.path().join("incoming_shared");

    let shared_observed = midpackage_observations(&shared_store, frames, || {
        let out = ingest_package(
            IngestConn::Shared(&shared_store),
            &shared_incoming,
            &pkg_dir,
            &announce,
            PEER_HEX,
            &announce.package_id.0,
            None,
            None,
        )
        .unwrap();
        assert_eq!(out.ingested, CONN_TEST_FRAMES as u32, "all frames ingested");
    });

    // The control (and the RED this test was written against): the pre-W2-T2.1
    // shape, where the CALLER holds `lock_conn()` for the whole package and hands
    // ingest a `Borrowed` connection. The competitor is then blocked from the
    // first frame to the last, so it can never observe a partial catalog — 0 by
    // construction, whatever the machine's timing.
    let control_catalog = tmp.path().join("catalog_control.db");
    let _control_db = crate::db::Database::new(control_catalog.clone()).unwrap();
    let control_store = CatalogSyncStore::open(&control_catalog).unwrap();
    let control_incoming = tmp.path().join("incoming_control");

    let control_observed = midpackage_observations(&control_store, frames, || {
        let conn = control_store.lock_conn();
        let out = ingest_package(
            IngestConn::Borrowed(&conn),
            &control_incoming,
            &pkg_dir,
            &announce,
            PEER_HEX,
            &announce.package_id.0,
            None,
            None,
        )
        .unwrap();
        assert_eq!(out.ingested, CONN_TEST_FRAMES as u32, "all frames ingested");
    });

    assert_eq!(
        control_observed, 0,
        "control premise: a whole-package guard makes a partial catalog unobservable"
    );
    // >= 1, deliberately: ONE observation of a partial catalog already proves
    // the guard is released between frames — the control above proves a
    // whole-package guard makes even one observation impossible. Requiring more
    // only re-introduces scheduler-load sensitivity (the full --workspace run
    // saturates every core), which is what flaked here once.
    assert!(
        shared_observed >= 1,
        "a competing thread must acquire the store connection mid-package \
         (partial-catalog acquisitions: {shared_observed} with Shared, \
         {control_observed} with a whole-package guard)"
    );
}

/// Behavior-neutrality pin for W2 T2.1: the same multi-frame package ingested
/// through `IngestConn::Borrowed` (one caller-owned connection) and through
/// `IngestConn::Shared` (the store locked per frame) must produce the same
/// outcome counts, the same receipts, and the same catalog/landing rows.
#[test]
fn ingest_shared_conn_matches_borrowed_outcome() {
    let tmp = TempDir::new().unwrap();
    let (pkg_dir, announce) = build_multi_frame_package(tmp.path(), 3, 64);

    // A — Borrowed, against a plain caller-owned catalog connection.
    let borrowed_catalog = tmp.path().join("catalog_borrowed.db");
    let borrowed_db = crate::db::Database::new(borrowed_catalog.clone()).unwrap();
    let borrowed_incoming = tmp.path().join("incoming_borrowed");
    let borrowed_out = {
        let conn = borrowed_db.conn();
        ingest_package(
            IngestConn::Borrowed(&conn),
            &borrowed_incoming,
            &pkg_dir,
            &announce,
            PEER_HEX,
            &announce.package_id.0,
            None,
            None,
        )
        .unwrap()
    };

    // B — Shared, against a real store that locks per frame.
    let shared_catalog = tmp.path().join("catalog_shared.db");
    let _shared_db = crate::db::Database::new(shared_catalog.clone()).unwrap();
    let shared_store = CatalogSyncStore::open(&shared_catalog).unwrap();
    let shared_incoming = tmp.path().join("incoming_shared");
    let shared_out = ingest_package(
        IngestConn::Shared(&shared_store),
        &shared_incoming,
        &pkg_dir,
        &announce,
        PEER_HEX,
        &announce.package_id.0,
        None,
        None,
    )
    .unwrap();

    // Same aggregate outcome.
    assert_eq!(shared_out.ingested, 3, "premise: every frame ingests");
    let counts =
        |o: &super::ingest::IngestOutcome| (o.ingested, o.duplicate, o.skipped_older, o.rejected);
    assert_eq!(
        counts(&borrowed_out),
        counts(&shared_out),
        "outcome counts identical"
    );
    assert_eq!(
        receipt_fingerprint(&borrowed_out.receipts),
        receipt_fingerprint(&shared_out.receipts),
        "receipts identical"
    );

    // Same catalog rows (meaningful columns only — ids/created_at are per-run).
    let borrowed_conn = borrowed_db.conn();
    let shared_conn = shared_store.lock_conn();
    for (sql, cols, what) in [
        ("SELECT filename, size, format, content_hash FROM files ORDER BY filename", 4, "files"),
        (
            "SELECT uuid, object, imagetyp, exptime, filter, instrume, telescop, gain, \"offset\", \
             binning, naxis1, naxis2, date_obs, updated_at FROM frames ORDER BY uuid",
            14,
            "frames",
        ),
        ("SELECT frame_uuid, xxh3, outcome FROM sync_receipts ORDER BY frame_uuid", 3, "sync_receipts"),
        (
            "SELECT frame_uuid, filename, object, peer_device, direction, bytes, outcome \
             FROM sync_history ORDER BY frame_uuid",
            7,
            "sync_history",
        ),
    ] {
        assert_eq!(
            rows_as_strings(&borrowed_conn, sql, cols),
            rows_as_strings(&shared_conn, sql, cols),
            "{what} rows must be identical across IngestConn variants"
        );
    }

    // Same landed files, at the same paths relative to each incoming root.
    assert_eq!(
        landed_rel_paths(&borrowed_conn, &borrowed_incoming),
        landed_rel_paths(&shared_conn, &shared_incoming),
        "landed layout identical"
    );
    assert_eq!(
        landed_rel_paths(&shared_conn, &shared_incoming).len(),
        3,
        "premise: three files landed"
    );
}

/// Stable text form of a receipt list (order-independent), for cross-run equality.
fn receipt_fingerprint(receipts: &[FrameReceipt]) -> Vec<String> {
    let mut out: Vec<String> = receipts
        .iter()
        .map(|r| format!("{}|{}|{:?}", r.frame_uuid, r.xxh3, r.outcome))
        .collect();
    out.sort();
    out
}

/// Render `cols` columns of every row of `sql` as `Value`-debug text — a
/// schema-agnostic row fingerprint for comparing two catalogs.
fn rows_as_strings(conn: &Connection, sql: &str, cols: usize) -> Vec<String> {
    let mut stmt = conn.prepare(sql).unwrap();
    let rows = stmt
        .query_map([], |r| {
            let mut parts = Vec::with_capacity(cols);
            for i in 0..cols {
                let v: rusqlite::types::Value = r.get(i)?;
                parts.push(format!("{v:?}"));
            }
            Ok(parts.join("|"))
        })
        .unwrap();
    rows.map(|r| r.unwrap()).collect()
}

/// Every `files.path`, relative to `incoming` and asserted to exist on disk.
fn landed_rel_paths(conn: &Connection, incoming: &Path) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT path FROM files ORDER BY path")
        .unwrap();
    let mut out: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|p| {
            let p = p.unwrap();
            let path = Path::new(&p);
            assert!(path.exists(), "landed file must exist on disk: {p}");
            path.strip_prefix(incoming)
                .unwrap_or_else(|_| panic!("landed file must be under the incoming root: {p}"))
                .to_string_lossy()
                .to_string()
        })
        .collect();
    out.sort();
    out
}

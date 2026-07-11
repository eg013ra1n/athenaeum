# Sync Phase 2B — mirror landing (receiver side)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The receiver lands accepted files mirroring the sender's directory tree — `<incoming_root>/<sender-slug>/<rel_path>` — instead of the flat `<device_short>/<date>/<basename>`. The sender prefix is derived from the **authenticated** peer node id (not the manifest-declared `origin_device`), keeping it out of attacker control.

**Architecture:** One change in `crates/athenaeum-core/src/sync/ingest.rs::land_payload`, plus a `sanitize_slug` helper. The full `rel_path` (already `validate_rel_path`-guarded upstream in `process_frame`) becomes the path under the per-sender folder. Core-only, TDD in `ingest_tests.rs`.

**Tech Stack:** Rust, `athenaeum-core`. Tests: `cargo test -p athenaeum-core`.

**Spec:** `docs/superpowers/specs/2026-07-11-sync-model-phase1-design.md` §6 (mirror landing) and §5 (receiver-resolved, sanitized slug — path-safety). Depends on Plan 2A (allow-list=account, already on `0.4.0`).

**Scope note:** In this plan the slug is the **authenticated peer node id** (sanitized, shortened) — safe and receiver-controlled, but hex, not a friendly name. Plan 2C threads a node-id→device-name resolver so the slug becomes the friendly node name; `sanitize_slug` is written now and reused there. The *sender* populating `rel_path` with real sub-directory structure (Perseus / `api::sync`) is also Plan 2C; this plan makes the receiver mirror **whatever** `rel_path` it is given (tested with a nested fixture).

## Global Constraints

- One production file: `crates/athenaeum-core/src/sync/ingest.rs`. Tests in `crates/athenaeum-core/src/sync/ingest_tests.rs`.
- The sender prefix MUST come from the authenticated `peer_device` (the hex node id passed into `ingest_package`), NOT from `record.origin_device` (manifest-declared, attacker-influenceable).
- `rel_path` safety is already enforced by `package::validate_rel_path` in `process_frame` (no `..`/root/backslash/drive) — do not weaken or duplicate it; the landing join relies on it.
- Preserve the existing tmp-copy + atomic-rename and the `unique_path` collision suffix.
- Commit author is the repo git user (`eg013ra1n`); **no Claude co-author/footer**.
- `cargo test -p athenaeum-core` green + `cargo build -p athenaeum-core` warning-free before commit (remove `short_device` if it becomes unused).

---

### Task 1: `land_payload` mirrors the sender tree under an authenticated-peer slug

**Files:**
- Modify: `crates/athenaeum-core/src/sync/ingest.rs` (`land_payload` signature + body; add `sanitize_slug`; thread `sender_slug` through `ingest_package` → `process_frame`; drop `short_device`)
- Test: `crates/athenaeum-core/src/sync/ingest_tests.rs`

**Interfaces:**
- Produces: `fn sanitize_slug(raw: &str) -> String` — a filesystem-safe single path segment: lowercase; every char outside `[a-z0-9._-]` becomes `-`; runs of `-` collapse; leading/trailing `-`/`.` trimmed; capped at 24 chars; empty result → `"node"`. Consumed here and by Plan 2C (applied to a resolved device name).
- Changes: `land_payload(incoming_root, payload, record, sender_slug: &str)` (was `(…, record, snapshot)`); `process_frame(…, sender_slug: &str, …)`; `ingest_package` computes the slug once from `peer_device`.

- [ ] **Step 1: Write the failing test**

Add to `crates/athenaeum-core/src/sync/ingest_tests.rs`. It builds a package whose manifest `rel_path` is **nested** and whose `origin_device` is a DIFFERENT (decoy) value than the sending peer, then asserts the landed path mirrors `rel_path` under the *authenticated* peer's slug:

```rust
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
        frame_meta: serde_json::json!({ "object": "M31" }),
        analysis: None,
        app_version: "test".to_string(),
    };
    let pkg_dir = root.join(format!("pkg-{frame_uuid}"));
    let announce = write_package(&pkg_dir, vec![(src, record)]).unwrap();
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
    sender.serve(&announce, &pkg_dir).await.unwrap();
    sender.announce(receiver_node, &announce).await.unwrap();
    let receipts =
        wait_for_ack(&mut sender_events, &announce.package_id.0, Duration::from_secs(5)).await;
    assert!(matches!(receipts[0].outcome, ReceiptOutcome::Ingested));

    // The slug is the authenticated sender node id, sanitized (NOT the decoy).
    let slug = super::ingest::sanitize_slug(&node_id_hex(&sender_node));
    let expected = incoming.join(&slug).join("M31/2026-07-10/lights/L_0001.fits");
    assert!(expected.exists(), "file must mirror rel_path under the peer slug: {}", expected.display());

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
    assert!(path.ends_with("M31/2026-07-10/lights/L_0001.fits"), "catalog path mirrors rel_path: {path}");
}
```

If `node_id_hex` / `NodeId` aren't already imported in the test module, they are — the file already uses `NodeId` and other tests use `node_id_hex` via `super`/`crate::sync`. If `sanitize_slug` needs to be reachable as `super::ingest::sanitize_slug`, make it `pub(crate)` in Step 3.

- [ ] **Step 2: Run it, confirm it fails**

Run: `cargo test -p athenaeum-core --lib ingest_mirrors_rel_path_under_authenticated_peer_slug`
Expected: FAIL — with the current `land_payload`, the file lands at `<incoming>/<short_device(origin_device="deadbeef…")>/<date>/L_0001.fits` (flattened, under the DECOY origin, with a date dir), so `expected` doesn't exist and the decoy slug folder DOES. (Also `sanitize_slug` doesn't compile yet → build error first; add the fn in Step 3 and re-run.)

- [ ] **Step 3: Add `sanitize_slug`, rewrite `land_payload`, thread the slug**

In `crates/athenaeum-core/src/sync/ingest.rs`:

Add the helper (near `filename_of`):

```rust
/// A filesystem-safe single path segment derived from a display string (the
/// per-sender landing-folder prefix). Lowercase; any char outside
/// `[a-z0-9._-]` → `-`; runs of `-` collapse; leading/trailing `-`/`.` trimmed;
/// capped at 24 chars; empty → `"node"`. Applied to the authenticated peer node
/// id here (Plan 2C applies it to a resolved device name instead).
pub(crate) fn sanitize_slug(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(24));
    let mut prev_dash = false;
    for ch in raw.chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
            out.push(c);
            prev_dash = false;
        } else {
            if !prev_dash {
                out.push('-');
            }
            prev_dash = true;
        }
        if out.len() >= 24 {
            break;
        }
    }
    let trimmed = out.trim_matches(|c| c == '-' || c == '.');
    if trimmed.is_empty() {
        "node".to_string()
    } else {
        trimmed.to_string()
    }
}
```

Replace `land_payload` (drop `snapshot`, take `sender_slug`, mirror `rel_path`):

```rust
/// Land an accepted payload mirroring the sender's tree under
/// `<incoming_root>/<sender_slug>/<rel_path>`, tmp-copy + atomic rename,
/// collision-suffixed. `sender_slug` is derived from the AUTHENTICATED peer node
/// id (Plan 2B); `rel_path` is `validate_rel_path`-guarded in `process_frame`, so
/// the join cannot escape `<incoming_root>/<sender_slug>/`. Returns the final path.
fn land_payload(
    incoming_root: &Path,
    payload: &Path,
    record: &ManifestRecord,
    sender_slug: &str,
) -> Result<PathBuf> {
    let dest = unique_path(&incoming_root.join(sender_slug).join(Path::new(&record.rel_path)));
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create landing dir {}", parent.display()))?;
    }

    // tmp + atomic rename: copy to a sibling temp, then rename into place.
    let tmp = dest.with_extension(format!(
        "{}.tmp",
        dest.extension().and_then(|e| e.to_str()).unwrap_or("part")
    ));
    std::fs::copy(payload, &tmp)
        .with_context(|| format!("copy payload to {}", tmp.display()))?;
    std::fs::rename(&tmp, &dest)
        .with_context(|| format!("rename landed file into {}", dest.display()))?;
    Ok(dest)
}
```

Thread the slug. In `ingest_package`, compute it once (the peer is the same for the whole package) right after reading the manifest:

```rust
    let sender_slug = sanitize_slug(peer_device);
```

and pass `&sender_slug` into each `process_frame(...)` call. Change `process_frame`'s signature to accept `sender_slug: &str` and pass it to `land_payload` in place of `&snapshot`:

```rust
    let landed = land_payload(incoming_root, &payload, record, sender_slug)
        .with_context(|| format!("land payload {}", record.rel_path))?;
```

`process_frame` still uses `snapshot` for the catalog rows — keep that; only the `land_payload` call loses it. Delete the now-unused `short_device` fn (grep confirms `land_payload` was its sole caller). `filename_of` stays (still used by `insert_ingested_rows` / `received_history`).

- [ ] **Step 4: Run the test + a slug unit test**

Add a focused `sanitize_slug` unit test in the ingest_tests module:

```rust
#[test]
fn sanitize_slug_is_path_safe() {
    assert_eq!(super::ingest::sanitize_slug("Studio Mac"), "studio-mac");
    assert_eq!(super::ingest::sanitize_slug("../../etc"), "etc");   // separators/dots → safe
    assert_eq!(super::ingest::sanitize_slug("a/b\\c:d"), "a-b-c-d");
    assert_eq!(super::ingest::sanitize_slug(""), "node");
    assert_eq!(super::ingest::sanitize_slug("!!!"), "node");
    // hex node id stays hex, capped.
    let s = super::ingest::sanitize_slug(&"ab".repeat(32));
    assert!(s.len() <= 24 && s.chars().all(|c| c.is_ascii_hexdigit()));
}
```

Run: `cargo test -p athenaeum-core --lib ingest_mirrors_rel_path_under_authenticated_peer_slug sanitize_slug_is_path_safe`
Expected: PASS.

- [ ] **Step 5: Run the full suite**

Run: `cargo test -p athenaeum-core` and `cargo build -p athenaeum-core`
Expected: all green (existing ingest tests that asserted the old `<device>/<date>/<basename>` layout — search `ingest_tests.rs` for `count_files`, `dir_a`, landing-path assertions — must be updated to the new `<slug>/<rel_path>` layout; the live-resolver test `receiver lands under the designated incoming root` still holds since only the sub-path changed). No unused-fn/import warnings (`short_device` removed).

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-core/src/sync/ingest.rs crates/athenaeum-core/src/sync/ingest_tests.rs
git commit -m "feat(sync): mirror sender tree under authenticated-peer slug on landing"
```

---

## Self-Review

**Spec coverage:** §6 mirror landing (`<incoming_root>/<sender-slug>/<rel_path>`) → Task 1. §5 path-safe slug from a receiver-controlled source (authenticated peer, not manifest `origin_device`) → `sanitize_slug` + the `sender_slug` threading. The friendly-name resolver and the sender-side `rel_path` population are explicitly Plan 2C (noted in scope). ✅

**Placeholder scan:** every step has complete code; the "update existing landing-path assertions" in Step 5 names how to find them (search `count_files`/`dir_a`) rather than leaving a TODO.

**Type consistency:** `sanitize_slug(&str) -> String`; `land_payload(&Path, &Path, &ManifestRecord, &str)`; `process_frame(…, sender_slug: &str, …)`; `ingest_package` computes `sender_slug` from `peer_device`. Consistent across definition and call sites.

---

## Execution Handoff

Plan complete. Two options:
1. **Subagent-Driven (recommended)** — fresh subagent per task, review after (`superpowers:subagent-driven-development`).
2. **Inline** — in this session with checkpoints (`superpowers:executing-plans`).

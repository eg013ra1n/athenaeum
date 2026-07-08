# Stage 1.5 — Sync Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop the blob-store disk leak on both sync roles, land received files under a user-designated scan root, give Perseus multiple capture directories plus a local web status page with manual delete and retention editing, and polish transfer history (device names, duration, safe-to-delete badge).

**Architecture:** Four workstreams on the shipped Stage I sync stack (spec: `docs/superpowers/specs/2026-07-08-stage15-sync-hardening-design.md`). Blob GC = new `SharingTransport::release` + deterministic `pkg/<package_id>` tags + iroh-blobs GC loop enabled at store creation + delete-all-tags startup sweep. Landing = new `sync_incoming`/`collaboration` scan-root kinds + a per-package resolver closure threaded into the receiver. Perseus web = embedded axum router over the existing engine/store handles, `toml_edit` write-back for retention, `tokio::sync::watch` for live policy updates.

**Tech Stack:** Rust (axum 0.8, toml_edit 0.23, iroh 1.0.2 / iroh-blobs 0.103 — pinned, do not bump), React/TS frontend, SQLite.

## Global Constraints

- **Two backends in sync:** every new Tauri command (`crates/athenaeum-tauri/src/commands/<domain>.rs`) gets its Axum mirror (`crates/athenaeum-web/src/routes/<domain>.rs`) in the same task, registered in `invoke_handler![]` (`crates/athenaeum-tauri/src/lib.rs`) and `build_router` (`crates/athenaeum-web/src/routes/mod.rs`). `#[tracing::instrument(skip_all, err)]` on every command boundary.
- **Headless build must keep compiling:** `cargo check -p athenaeum-core --no-default-features` after every core change (Perseus depends on it).
- **Logging:** `tracing` only, message = short stable phrase + snake_case fields (`info!(package_id, count = n, "blobs released")`), never data in the message. Zero `println!` outside test/CLI exemptions.
- **Errors:** `anyhow::Result` in core, `.map_err(|e| e.to_string())` at Tauri boundary, `(StatusCode, String)` in web. Never swallow — log before returning.
- **Serde:** `#[serde(rename_all = "camelCase")]` on every IPC type; new IPC types added to `crates/athenaeum-core/src/ts_export.rs` registry; verify `src/types/models.ts`.
- **Frontend:** backend access via `src/api/` only; design tokens (`bg-surface`, `text-content-muted`, `text-error`, …); notifications via `notify()` from `useNotifications()`.
- **Retention safety invariant (do not weaken):** only `confirmed` packages are ever deletable; dry-run defaults on; live deletion requires the two TOML keys — the web PUT endpoint must not be able to touch `i_have_verified_the_soak`.
- **Gates per task:** `cargo test -p athenaeum-core` (and `-p perseus` where touched), `cargo build --workspace --all-targets` at least at Tasks 6 and 13, `npx tsc --noEmit` for frontend tasks. `cargo clippy -D warnings` is NOT a gate (pre-existing debt). `cargo fmt` only via `rustfmt <files>` on touched files.
- **Commits:** author `eg013ra1n <vilen.sharifov@gmail.com>`, no AI co-author, branch `0.4.0`.
- Versions stay pinned: `iroh = "=1.0.2"`, `iroh-blobs = "=0.103.0"`. New perseus deps: `axum = "0.8"` (matches athenaeum-web), `toml_edit = "0.23"` (already in the workspace lockfile).

---

### Task 1: `SharingTransport::release` + deterministic package tags + GC-enabled blob store

The core of the leak fix. Today `import_package_collection` pins each package collection with an **auto-named tag whose name is discarded** (`blobs.rs:87-91` uses `tags().create(...)`), the receiver's downloaded blobs are **never tagged at all**, and `FsStore::load` runs with `gc: None` so nothing is ever reclaimed. This task: deterministic tag names, a `release` trait method, receiver-side fetch tagging, and the GC loop enabled.

**Files:**
- Modify: `crates/athenaeum-core/src/sharing/mod.rs` (trait, lines 38-71)
- Modify: `crates/athenaeum-core/src/sharing/loopback.rs`
- Modify: `crates/athenaeum-core/src/sharing/iroh/mod.rs` (struct lines 100-124, store creation lines 147-156, `serve` 341-354, `fetch` ~316-339)
- Modify: `crates/athenaeum-core/src/sharing/iroh/blobs.rs` (`import_package_collection` 62-102, `fetch_collection_to_dir` 119-171)
- Test: `crates/athenaeum-core/src/sharing/tests.rs` (loopback), `crates/athenaeum-core/src/sharing/iroh/tests.rs`

**Interfaces:**
- Produces: `SharingTransport::release(&self, package_id: &PackageId) -> anyhow::Result<()>` (Tasks 2, 3 call it); `pub(crate) fn package_tag(package_id: &PackageId) -> String` in `iroh/mod.rs`; `import_package_collection(store, pkg_dir, tag: &str)` and `fetch_collection_to_dir(store, endpoint, provider, root_hash, tag: &str, dest_dir)` new signatures.

- [ ] **Step 1: Write the failing loopback test**

In `crates/athenaeum-core/src/sharing/tests.rs`, next to `loopback_announce_fetch_ack_roundtrip` (reuse its setup helpers verbatim — network, two endpoints, `build_package`-style package dir):

```rust
#[tokio::test]
async fn loopback_release_makes_package_unfetchable() {
    let net = LoopbackNetwork::new();
    let a = net.endpoint();
    let b = net.endpoint();
    let (ia, ib) = (a.start().await.unwrap(), b.start().await.unwrap());
    let (dir, announce) = one_frame_package(); // existing helper pattern in this file
    a.serve(&announce, &dir).await.unwrap();

    // Served → fetch succeeds.
    let dest1 = tempfile::tempdir().unwrap();
    b.fetch(ia.node_id, &announce, dest1.path()).await.unwrap();

    // Released → the same fetch now fails with "not served".
    a.release(&announce.package_id).await.unwrap();
    let dest2 = tempfile::tempdir().unwrap();
    let err = b.fetch(ia.node_id, &announce, dest2.path()).await.unwrap_err();
    assert!(err.to_string().contains("not served"), "got: {err}");

    // Idempotent: releasing again (or an unknown id) is Ok.
    a.release(&announce.package_id).await.unwrap();
    let _ = ib;
}
```

Adapt helper names to what `sharing/tests.rs` actually uses (it has the loopback fixtures for `loopback_announce_fetch_ack_roundtrip`); the assertions above are the contract.

- [ ] **Step 2: Run it — expect a compile failure (`release` not on the trait)**

Run: `cargo test -p athenaeum-core sharing::tests::loopback_release -- --nocapture`
Expected: FAIL — `no method named release`.

- [ ] **Step 3: Add the trait method (no default impl — every transport must decide)**

`crates/athenaeum-core/src/sharing/mod.rs`, after `ack` (line ~63):

```rust
    /// Drop the local payload data for `package_id` — the package reached a
    /// terminal state (confirmed / failed / cancelled on the sender; acked on
    /// the receiver) and its blobs must not outlive it. Idempotent: releasing
    /// an unknown or already-released package is Ok(()). Never fails the
    /// caller's state transition — callers log-and-continue on Err.
    async fn release(&self, package_id: &PackageId) -> anyhow::Result<()>;
```

Loopback impl in `crates/athenaeum-core/src/sharing/loopback.rs` (inside the `#[async_trait] impl`, next to `serve` at line 246 — same registry/inbox access pattern):

```rust
    async fn release(&self, package_id: &PackageId) -> anyhow::Result<()> {
        let mut reg = self.registry.lock().expect("registry mutex poisoned");
        if let Some(inbox) = reg.get_mut(&self.node_id) {
            let removed = inbox.served.remove(&package_id.0).is_some();
            tracing::debug!(package_id = %package_id.0, removed, "loopback released package");
        }
        Ok(())
    }
```

- [ ] **Step 4: Run the loopback test — expect PASS; iroh won't compile yet, so implement iroh in the same pass**

`crates/athenaeum-core/src/sharing/iroh/mod.rs`:

(a) Tag naming helper (top-level, near `SYNC_ALPN`):

```rust
/// Deterministic blob-store tag for a package collection. `release` deletes by
/// this exact name, so both the import (serve) and download (fetch) sides pin
/// with it — never with an auto-named tag.
pub(crate) fn package_tag(package_id: &PackageId) -> String {
    format!("pkg/{}", package_id.0)
}
```

(b) Enable GC in the Fs store arm (lines 147-156). iroh-blobs 0.103 has **no public one-shot GC call** — the loop must be configured at store creation (`FsStore::load` hardcodes `gc: None`):

```rust
use iroh_blobs::store::fs::options::Options as FsOptions; // adjust to the actual re-export path
use iroh_blobs::store::GcConfig;

const GC_INTERVAL: std::time::Duration = std::time::Duration::from_secs(900);

let store: Store = match store {
    BlobStore::Memory => MemStore::new().into(),
    BlobStore::Fs(dir) => {
        let blob_dir = dir.join("sync_blobs");
        // Mirror FsStore::load's internals (fs.rs:1390-1394) but with GC on:
        // load() would pass gc: None and no GC loop would ever run.
        std::fs::create_dir_all(&blob_dir)
            .with_context(|| format!("create blob dir {}", blob_dir.display()))?;
        let db_path = blob_dir.join("blobs.db");
        let mut options = FsOptions::new(&blob_dir);
        options.gc = Some(GcConfig { interval: GC_INTERVAL, add_protected: None });
        FsStore::load_with_opts(db_path, options)
            .await
            .with_context(|| format!("open blob store {}", blob_dir.display()))?
            .into()
    }
};
```

(Verify the `Options` import path against `~/.cargo/registry/src/index.crates.io-*/iroh-blobs-0.103.0/src/store/mod.rs` re-exports — `GcConfig` is re-exported from `store`, `Options` lives under `store::fs::options`. Only 900 s interval + `add_protected: None`; a partial download older than one interval may be collected before a resume — that degrades resume to a re-download, never loses data; say this in a comment.)

(c) `serve` (lines 341-354) passes the tag; `release` impl:

```rust
    async fn serve(&self, pkg: &PackageAnnounce, src_dir: &Path) -> Result<()> {
        let tag = package_tag(&pkg.package_id);
        let hash = blobs::import_package_collection(&self.store, src_dir, &tag).await?;
        self.served
            .lock()
            .expect("served mutex poisoned")
            .insert(pkg.package_id.0.clone(), hash);
        // ... existing debug log unchanged
        Ok(())
    }

    async fn release(&self, package_id: &PackageId) -> Result<()> {
        self.served
            .lock()
            .expect("served mutex poisoned")
            .remove(&package_id.0);
        let removed = self
            .store
            .tags()
            .delete(package_tag(package_id))
            .await
            .map_err(|e| anyhow::anyhow!("delete package tag: {e}"))?;
        tracing::debug!(package_id = %package_id.0, tags_removed = removed, "iroh released package");
        Ok(())
    }
```

(`tags().delete` returns the removed count and does NOT error on a missing tag — idempotency comes free.)

(d) `fetch` (the trait impl around line 316) passes the tag into `fetch_collection_to_dir` so downloaded data is pinned from download-complete until the receiver releases after ack.

(e) `crates/athenaeum-core/src/sharing/iroh/blobs.rs` — both signatures grow a `tag: &str` param:

In `import_package_collection(store: &Store, pkg_dir: &Path, tag: &str)`, replace lines 87-91:

```rust
    store
        .tags()
        .set(tag, tag_tt.hash_and_format())
        .await
        .context("tag package collection")?;
```

(`tag_tt` = the `TempTag` currently named `tag` at line 81 — rename it `tag_tt` to free the name.) `tags().set` overwrites an existing same-name tag — a re-serve of the same package id re-points the tag, no leak.

In `fetch_collection_to_dir(store, endpoint, provider, root_hash: Hash, tag: &str, dest_dir: &Path)`, right after the `download(...)` await (line 131):

```rust
    // Pin the downloaded collection until the caller releases it (post-ack).
    // Between download-complete and this set the data is untagged — the 900 s
    // GC interval makes that window irrelevant in practice.
    store
        .tags()
        .set(tag, HashAndFormat::hash_seq(root_hash))
        .await
        .context("tag fetched collection")?;
```

- [ ] **Step 5: Write the failing iroh tag-lifecycle test**

In `crates/athenaeum-core/src/sharing/iroh/tests.rs` (reuse `mem_transport()`, `start_and_pair`, `build_package` — lines 45-94):

```rust
#[tokio::test]
async fn release_deletes_package_tags_on_both_sides() {
    let provider = mem_transport().await;
    let receiver = mem_transport().await;
    let (ip, _ir) = start_and_pair(&provider, &receiver).await;

    let tmp = tempfile::tempdir().unwrap();
    let (dir, announce) = build_package(tmp.path(), "uuid-gc-1", "gc.fits", "M1", 4096);
    provider.serve(&announce, &dir).await.unwrap();

    let tag = package_tag(&announce.package_id);
    // Provider pinned under the deterministic name.
    assert!(provider.store.tags().get(tag.as_bytes()).await.unwrap().is_some());

    let dest = tempfile::tempdir().unwrap();
    receiver.fetch(ip.node_id, &announce, dest.path()).await.unwrap();
    // Receiver pinned the downloaded collection under the same name.
    assert!(receiver.store.tags().get(tag.as_bytes()).await.unwrap().is_some());

    provider.release(&announce.package_id).await.unwrap();
    receiver.release(&announce.package_id).await.unwrap();
    assert!(provider.store.tags().get(tag.as_bytes()).await.unwrap().is_none());
    assert!(receiver.store.tags().get(tag.as_bytes()).await.unwrap().is_none());

    // Idempotent second release.
    provider.release(&announce.package_id).await.unwrap();
}
```

(The tests module is a descendant of `sharing::iroh`, so `provider.store` private-field access works — same pattern as `fetch_rejects_traversal_entry_names`, tests.rs:489.)

- [ ] **Step 6: Run the sharing test suites**

Run: `cargo test -p athenaeum-core sharing:: -- --nocapture`
Expected: all pass, including the two new tests and every pre-existing loopback/iroh test (the signature changes touch only in-crate callers: `serve`/`fetch` impls and the traversal test's direct `fetch_collection_to_dir` call — update that call site with a tag arg).

- [ ] **Step 7: Headless + workspace check, commit**

Run: `cargo check -p athenaeum-core --no-default-features && cargo build --workspace 2>&1 | tail -3`

```bash
git add crates/athenaeum-core/src/sharing/
git commit -m "feat(sharing): SharingTransport::release + deterministic pkg/ tags + blob-store GC loop"
```

---

### Task 2: Startup sweep — delete all tags when the transport starts

Any tag that exists when a process starts is stale by construction: `PackageId`s are minted per-announce and never persisted (engine.rs:841 — crash-resume re-announces with a *fresh* id and re-serves from the source dir, re-creating the tag), and receiver-side fetch tags never outlive a fetch→ack cycle. So the sweep is simply `delete_all` before anything is served — and it retroactively cleans everything both live deployments accumulated under the old auto-named scheme.

**Files:**
- Modify: `crates/athenaeum-core/src/sharing/iroh/mod.rs` (`start` method)
- Test: `crates/athenaeum-core/src/sharing/iroh/tests.rs`

**Interfaces:**
- Consumes: `store.tags().delete_all() -> RequestResult<u64>` (iroh-blobs).
- Produces: nothing new — behavior only.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn start_sweeps_stale_tags() {
    // Persistent store so tags survive the restart (pattern from
    // iroh_resume_after_endpoint_restart, tests.rs:211).
    let home = tempfile::tempdir().unwrap();
    let t1 = IrohTransport::new(random_secret(), RelayMode::Disabled, BlobStore::Fs(home.path().to_path_buf()))
        .await
        .unwrap();
    t1.start().await.unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let (dir, announce) = build_package(tmp.path(), "uuid-sweep-1", "s.fits", "M1", 2048);
    t1.serve(&announce, &dir).await.unwrap();
    t1.shutdown().await;

    // New process over the same store: the old tag must be gone after start().
    let t2 = IrohTransport::new(random_secret(), RelayMode::Disabled, BlobStore::Fs(home.path().to_path_buf()))
        .await
        .unwrap();
    t2.start().await.unwrap();
    let tag = package_tag(&announce.package_id);
    assert!(t2.store.tags().get(tag.as_bytes()).await.unwrap().is_none());
    t2.shutdown().await;
}
```

- [ ] **Step 2: Run — expect FAIL (tag survives)**

Run: `cargo test -p athenaeum-core sharing::iroh::tests::start_sweeps_stale_tags`
Expected: FAIL on the `is_none` assert.

- [ ] **Step 3: Implement in `IrohTransport::start`**

At the top of `start()` (before the endpoint goes online / anything is served):

```rust
        // Startup sweep: every tag in this store is stale — PackageIds are
        // per-process (crash-resume re-announces with fresh ids and re-serves
        // from source dirs), and receiver fetch-tags never outlive an ack.
        // Also retires the pre-Stage-1.5 auto-named tags on existing stores.
        match self.store.tags().delete_all().await {
            Ok(removed) if removed > 0 => {
                tracing::info!(count = removed, "blob store startup sweep removed stale tags")
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "blob store startup sweep failed"),
        }
```

- [ ] **Step 4: Run the full iroh suite (sweep must not break Task 1's lifecycle test — its tags are created after `start`)**

Run: `cargo test -p athenaeum-core sharing::iroh::`
Expected: PASS all.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/sharing/iroh/
git commit -m "feat(sharing): startup tag sweep — retire stale package pins (incl. pre-1.5 auto tags)"
```

---

### Task 3: Engine + receiver release hooks

Wire `release` into every terminal transition. `Worker::handle_event` is deliberately synchronous (engine.rs:540-542) — hooks use fire-and-forget `tokio::spawn` on the cloned `Arc<dyn SharingTransport>`, so a release failure can never fail a state transition (log `warn` inside the spawned task).

**Files:**
- Modify: `crates/athenaeum-core/src/sync/engine.rs` (`on_ack` 582-619, `fail_package` 658-676, `cancel_package` 680-708)
- Modify: `crates/athenaeum-core/src/sync/receiver.rs` (`handle_announce` — after both ack paths: replay ~169-181, normal ~221-224)
- Test: `crates/athenaeum-core/src/sync/engine_tests.rs`, `crates/athenaeum-core/src/sharing/iroh/tests.rs` (`engine_suite_over_iroh` extension)

**Interfaces:**
- Consumes: `SharingTransport::release` (Task 1).
- Produces: behavior only — after `confirmed`/`failed`/`cancelled` (sender) and post-ack (receiver), the package's blobs are released.

- [ ] **Step 1: Write the failing engine test (loopback)**

In `engine_tests.rs`, reuse the existing happy-path fixture (the test that drives a package to `confirmed` over `LoopbackTransport`):

```rust
#[tokio::test]
async fn confirmed_package_is_released_from_transport() {
    // ... existing happy-path setup: store, loopback pair, engine, enqueue,
    //     wait_until state == confirmed (copy the body of the existing
    //     confirm test in this file) ...

    // After confirm, the sender must have released: a fresh fetch of the
    // same announce from the sender now fails "not served".
    let dest = tempfile::tempdir().unwrap();
    let err = receiver_transport
        .fetch(sender_node, &captured_announce, dest.path())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not served"), "got: {err}");
}
```

To capture the announce: the receiver side of the fixture already sees `TransportEvent::AnnounceReceived { announce, .. }` — keep a clone of it when the fixture acks. Because release is a spawned task, wrap the final assert in the file's `wait_until`-style poll (release lands within milliseconds; poll up to 5 s).

- [ ] **Step 2: Run — expect FAIL (fetch still succeeds after confirm)**

Run: `cargo test -p athenaeum-core sync::engine_tests::confirmed_package_is_released`
Expected: FAIL — fetch succeeds because nothing releases.

- [ ] **Step 3: Implement the three sender hooks**

Add a small private helper on `Worker`:

```rust
    /// Fire-and-forget blob release for a terminal package. Never blocks or
    /// fails the state transition that triggered it.
    fn spawn_release(&self, package_id: PackageId) {
        let transport = Arc::clone(&self.transport);
        tokio::spawn(async move {
            if let Err(e) = transport.release(&package_id).await {
                tracing::warn!(package_id = %package_id.0, error = %format!("{e:#}"), "blob release failed");
            }
        });
    }
```

Call sites:
1. `on_ack`, immediately after `self.append_confirmed_history(...)` (line 615): `self.spawn_release(pending.announce.package_id.clone());` (`pending` was just removed from the map at line 611 — it owns the announce).
2. `fail_package` (line 658): the `pending` entry may or may not hold a minted announce — `if let Some(p) = self.pending.get(&id) { self.spawn_release(p.announce.package_id.clone()); }` placed right after `set_state(id, OutboundState::Failed)` succeeds (line 664). Match the actual `Pending` shape: if `announce` is `Option`, go through it; if the entry is removed in this fn, hook before removal.
3. `cancel_package` — same pattern next to its `set_state` (line 698).

- [ ] **Step 4: Receiver hook**

In `receiver.rs::handle_announce`, after **both** successful `transport.ack(...)` awaits (the replay path ~line 171 and the normal path ~line 224):

```rust
    if let Err(e) = transport.release(&announce.package_id).await {
        tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "receiver blob release failed");
    }
```

(The replay path releases too — a lost-ack resend may have re-downloaded blobs; release is idempotent.)

- [ ] **Step 5: Extend `engine_suite_over_iroh` with the spec §6 post-confirm assertion**

In `iroh/tests.rs`, at the end of `engine_suite_over_iroh` (after its existing confirmed assertion), poll-assert both stores hold zero tags:

```rust
    wait_until(
        || async {
            let p = provider.store.tags().list().await.unwrap().count().await == 0; // adapt to the Stream API in scope
            let r = receiver.store.tags().list().await.unwrap().count().await == 0;
            p && r
        },
        IROH_WAIT,
    )
    .await;
```

(Adapt to the file's `wait_until` signature — it takes a sync predicate in some fixtures; if so, use `futures::executor::block_on` or restructure as a loop with `tokio::time::sleep`, matching neighboring test style. This satisfies the spec's "sender blob store empty, receiver blob store empty" — the loopback e2e harness has no blob store, so the iroh suite carries this assertion.)

- [ ] **Step 6: Run engine + iroh + full core suites**

Run: `cargo test -p athenaeum-core sync:: && cargo test -p athenaeum-core sharing:: && cargo test -p athenaeum-core`
Expected: PASS all (~650 tests).

- [ ] **Step 7: Commit**

```bash
git add crates/athenaeum-core/src/sync/ crates/athenaeum-core/src/sharing/iroh/tests.rs
git commit -m "feat(sync): release package blobs on confirmed/failed/cancelled and after receiver ack"
```

---

### Task 4: `sync_incoming` + `collaboration` scan-root kinds (core + both backends)

**Files:**
- Modify: `crates/athenaeum-core/src/api/scan_roots.rs` (`validate_scan_root_kind` 89-94, `check_library_root_uniqueness` 99-111, `guard_against_calibration_library_deletion` 412-428; new get/set/clear fns modeled on the calibration trio at 231-236 / 304-340 / adjacent clear)
- Modify: `crates/athenaeum-tauri/src/commands/scan_roots.rs`, `crates/athenaeum-tauri/src/lib.rs` (invoke_handler)
- Modify: `crates/athenaeum-web/src/routes/scan_roots.rs`, `crates/athenaeum-web/src/routes/mod.rs`
- Test: unit tests in `api/scan_roots.rs`'s existing `#[cfg(test)]` module

**Interfaces:**
- Produces (core api, all `(ctx: &ServiceContext, …)`):
  - `get_sync_incoming_dir(ctx) -> Result<Option<String>, ApiError>`
  - `set_sync_incoming_dir(ctx, path: String, policy: &PathPolicy) -> Result<String, ApiError>`
  - `clear_sync_incoming_dir(ctx) -> Result<(), ApiError>`
  - `get_collaboration_dir` / `set_collaboration_dir` / `clear_collaboration_dir` — same shapes.
  - `pub(crate) const SPECIAL_ROOT_KINDS: &[&str] = &["calibration_library", "sync_incoming", "collaboration"];`
- Task 5 consumes `get_sync_incoming_dir`; Task 6's UI calls the six commands.

- [ ] **Step 1: Write failing unit tests**

In the `api/scan_roots.rs` test module (reuse its `ServiceContext::new_for_tests` + tempdir fixtures used by the calibration-library tests):

```rust
#[test]
fn sync_incoming_root_set_get_clear_roundtrip() {
    // set → get returns the path and the root row has kind='sync_incoming'
    // second set of a different path → ApiError::Conflict (uniqueness)
    // clear → get returns None; the row is gone (or demoted, matching the
    //         calibration clear semantics — mirror them exactly)
}

#[test]
fn collaboration_root_uniqueness_independent_of_sync_incoming() {
    // one sync_incoming root AND one collaboration root can coexist;
    // a second collaboration root conflicts.
}

#[test]
fn special_roots_reject_plain_delete() {
    // delete_scan_root on a sync_incoming root → ApiError (guard), same as
    // the calibration guard behavior.
}
```

Fill the bodies by cloning the existing `calibration_library` tests in this file and swapping the kind + command names — the semantics are deliberately identical.

- [ ] **Step 2: Run — expect FAIL (fns don't exist)**

Run: `cargo test -p athenaeum-core api::scan_roots`

- [ ] **Step 3: Implement in core**

1. `validate_scan_root_kind` accepts the two new values:
```rust
fn validate_scan_root_kind(kind: &str) -> Result<(), ApiError> {
    match kind {
        "normal" | "calibration_library" | "sync_incoming" | "collaboration" => Ok(()),
        other => Err(ApiError::InvalidInput(format!("unknown scan root kind: {other}"))),
    }
}
```
2. Generalize uniqueness (keep the old fn name as a thin wrapper if other call sites exist, or update all call sites):
```rust
pub(crate) fn check_special_root_uniqueness(
    conn: &rusqlite::Connection,
    kind: &str,
) -> Result<(), ApiError> {
    if SPECIAL_ROOT_KINDS.contains(&kind) && crate::db::count_scan_roots_of_kind(conn, kind)? > 0 {
        return Err(ApiError::Conflict(format!(
            "A {kind} root already exists — only one is allowed"
        )));
    }
    Ok(())
}
```
3. The six new fns: copy `get_calibration_library_dir` / `set_calibration_library_dir` / `clear_calibration_library_dir` bodies, replacing the kind literal and user-facing strings ("Sync incoming folder", "Collaboration folder"). `set_*` routes through `add_scan_root(ctx, path, policy, Some("<kind>".into()))` exactly like the calibration setter (line 340).
4. Extend `guard_against_calibration_library_deletion` to guard all `SPECIAL_ROOT_KINDS` (rename to `guard_against_special_root_deletion`, update its message to name the kind and the matching clear command).

- [ ] **Step 4: Thin wrappers, both backends**

`crates/athenaeum-tauri/src/commands/scan_roots.rs` — six commands following the file's existing `set_calibration_library_dir` wrapper (line 57) exactly:

```rust
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_sync_incoming_dir(state: State<'_, AppState>, path: String) -> Result<String, String> {
    athenaeum_core::api::scan_roots::set_sync_incoming_dir(&state.ctx, path, &state.path_policy)
        .map_err(|e| e.to_string())
}
```

(…and get/clear + the collaboration triple; register all six in `invoke_handler![]` in `crates/athenaeum-tauri/src/lib.rs`.)

`crates/athenaeum-web/src/routes/scan_roots.rs` — six mirrors following the file's `set_calibration_library_dir` route; register `/api/get_sync_incoming_dir`, `/api/set_sync_incoming_dir`, `/api/clear_sync_incoming_dir`, `/api/get_collaboration_dir`, `/api/set_collaboration_dir`, `/api/clear_collaboration_dir` in `routes/mod.rs` next to the calibration routes (mod.rs:46-52).

- [ ] **Step 5: Run tests + workspace build**

Run: `cargo test -p athenaeum-core api::scan_roots && cargo build --workspace 2>&1 | tail -3`
Expected: PASS / clean build (proves both wrappers compile).

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-core/src/api/scan_roots.rs crates/athenaeum-tauri/ crates/athenaeum-web/
git commit -m "feat(scan-roots): sync_incoming + collaboration root kinds with per-kind uniqueness (both backends)"
```

---

### Task 5: Receiver lands under the designated `sync_incoming` root (live resolver)

Landing target becomes a **per-package resolved** closure, so designating/clearing the root takes effect on the next package without restarting the transport. Staging moves out of the user-visible tree: it stays under `<sync_dir>/staging/`.

**Files:**
- Modify: `crates/athenaeum-core/src/sync/receiver.rs` (`SyncReceiver::spawn` 103-137, `handle_announce` 142-253, `SyncRuntime::ensure_started` 308-353)
- Modify: `crates/athenaeum-core/src/api/sync.rs` (`autostart_if_enabled` ~269-288, `get_pairing_ticket` ~301-322)
- Test: `crates/athenaeum-core/tests/sync_e2e.rs`

**Interfaces:**
- Produces: `pub type IncomingResolver = std::sync::Arc<dyn Fn() -> PathBuf + Send + Sync>;` (in `sync/receiver.rs`); `SyncReceiver::spawn(store, staging_root: PathBuf, incoming: IncomingResolver, transport, emitter)`; `SyncRuntime::ensure_started(sync_dir, db_path, relay_mode, incoming: IncomingResolver, emitter)`.
- Consumes: `api::scan_roots::get_sync_incoming_dir` (Task 4).

- [ ] **Step 1: Write the failing e2e assertion**

In `tests/sync_e2e.rs`, before the receiver spawn (line ~187), designate a sync-incoming root on the primary context, and change the phase-1 landing assertions to expect files under it:

```rust
    let designated = tmp.path().join("designated_incoming");
    std::fs::create_dir_all(&designated).unwrap();
    athenaeum_core::api::scan_roots::add_scan_root(
        &primary_ctx,
        designated.to_string_lossy().into_owned(),
        &test_path_policy(), // whatever PathPolicy fixture the test tree uses
        Some("sync_incoming".to_string()),
    )
    .unwrap();
    // after phase 1 confirms:
    let landed: Vec<_> = walkdir::WalkDir::new(&designated)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .collect();
    assert_eq!(landed.len(), N, "all frames land under the designated sync_incoming root");
```

(If `walkdir` isn't a dev-dep of core, use a small recursive `std::fs` walk — three lines.) Keep a second, tiny test path for the fallback: a fresh context with **no** designated root must land under the passed staging default — cover that in a receiver unit test below instead of a second e2e run.

- [ ] **Step 2: Run — expect compile FAIL (spawn signature), then implement**

`receiver.rs`:

```rust
pub type IncomingResolver = std::sync::Arc<dyn Fn() -> PathBuf + Send + Sync>;
```

`SyncReceiver::spawn(store, staging_root: PathBuf, incoming: IncomingResolver, transport, emitter)`:
- create `staging_root` (was `incoming_root`) upfront;
- pass both into the loop task; `handle_announce(store, &staging_root, &incoming, transport, emitter, from, announce)`.

`handle_announce`:
- staging dir: `let staging = staging_root.join("staging").join(&package_id);` (replaces `incoming_root.join(".staging")…`);
- resolve landing root per package, immediately before the ingest spawn_blocking: `let incoming_root = incoming();` and pass that into `ingest::ingest_package` (whose signature is unchanged).

`SyncRuntime::ensure_started(&self, sync_dir, db_path, relay_mode, incoming: IncomingResolver, emitter)`:
- replace line 342 `let incoming_root = sync_dir.join("incoming");` with the caller-supplied resolver; pass `sync_dir.clone()` as `staging_root`.

`api/sync.rs`, both call sites build the resolver (they have `ctx`; capture the `Arc`ed DB handle the file already uses — mirror how `sync_paths`/`db(ctx)` obtains it):

```rust
    let (sync_dir, db_path) = sync_paths(ctx)?;
    let fallback = sync_dir.join("incoming");
    let db = db(ctx)?.clone(); // Arc'd handle — adapt to the file's actual accessor
    let incoming: crate::sync::receiver::IncomingResolver = std::sync::Arc::new(move || {
        let conn = db.conn();
        match crate::db::scan_root_path_of_kind(&conn, "sync_incoming") {
            Ok(Some(p)) => PathBuf::from(p),
            Ok(None) => fallback.clone(),
            Err(e) => {
                tracing::warn!(error = %e, "sync_incoming root lookup failed; landing in app-data fallback");
                fallback.clone()
            }
        }
    });
```

Add the tiny `scan_root_path_of_kind(conn, kind) -> Result<Option<String>>` helper in `crates/athenaeum-core/src/db/` next to `count_scan_roots_of_kind` (single `SELECT path FROM scan_roots WHERE kind = ?1 LIMIT 1`, `optional()`).

- [ ] **Step 3: Receiver fallback unit test**

Next to the receiver code (or in `engine_tests.rs` if receiver tests live there), a loopback-driven test: spawn `SyncReceiver` with a resolver that returns dir A, announce one package → lands under A; swap the resolver's target via an `Arc<Mutex<PathBuf>>` captured in the closure, announce a second package → lands under B. This pins the "live, per-package" semantics:

```rust
let target = Arc::new(Mutex::new(dir_a.clone()));
let resolver: IncomingResolver = {
    let t = Arc::clone(&target);
    Arc::new(move || t.lock().unwrap().clone())
};
// … announce pkg1, wait ingested, assert file under dir_a …
*target.lock().unwrap() = dir_b.clone();
// … announce pkg2, wait ingested, assert file under dir_b …
```

- [ ] **Step 4: Run e2e + core suite**

Run: `cargo test -p athenaeum-core --test sync_e2e && cargo test -p athenaeum-core`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/sync/ crates/athenaeum-core/src/api/sync.rs crates/athenaeum-core/src/db/ crates/athenaeum-core/tests/sync_e2e.rs
git commit -m "feat(sync): land received files under the designated sync_incoming root (live per-package resolution)"
```

---

### Task 6: Directory-manager UI sections + "unconfigured" notification (frontend)

**Files:**
- Create: `src/components/SpecialFolderSection.tsx` (generic, parameterized)
- Modify: `src/pages/FileManager.tsx` (mount two sections next to `CalibrationFolderSection`, line ~691)
- Modify: `src/contexts/TransfersContext.tsx` (unconfigured-root hint on received packages)
- Modify: `src/types/models.ts` only if `ScanRoot.kind` doc union needs the two new literals (comment-level)

**Interfaces:**
- Consumes: the six commands from Task 4 via `api.invoke`; `useNotifications().notify`.

- [ ] **Step 1: Build the generic section component**

`src/components/SpecialFolderSection.tsx` — clone the structure of `CalibrationFolderSection.tsx` (props `{ scanRoots, onRootsChanged }`, dir-picker via the existing desktop/web path input pattern in that file), parameterized:

```tsx
interface SpecialFolderSectionProps {
  title: string;                // "Sync Incoming Folder" | "Collaboration Folder"
  description: string;          // one-line purpose text
  kind: 'sync_incoming' | 'collaboration';
  getCommand: string;           // 'get_sync_incoming_dir' | 'get_collaboration_dir'
  setCommand: string;
  clearCommand: string;
  scanRoots: ScanRoot[];
  onRootsChanged: () => void;
}
```

Behavior identical to the calibration section: show current dir (from `api.invoke<string | null>(getCommand)`), Set/Change via `api.invoke(setCommand, { path })`, Clear via `clearCommand`, error surfacing via `notify({ tone: 'warning', … })`, design tokens throughout (`bg-surface`, `text-content-muted`). Sync section description: "Files received from your capture devices land here." Collaboration description: "Reserved for collaboration projects (Stage II) — received project contributions will be stored here."

- [ ] **Step 2: Mount in FileManager**

In `src/pages/FileManager.tsx` beside `<CalibrationFolderSection …>` (line ~691):

```tsx
<SpecialFolderSection
  title="Sync Incoming Folder"
  description="Files received from your capture devices land here."
  kind="sync_incoming"
  getCommand="get_sync_incoming_dir"
  setCommand="set_sync_incoming_dir"
  clearCommand="clear_sync_incoming_dir"
  scanRoots={scanRoots}
  onRootsChanged={reloadRoots}
/>
<SpecialFolderSection
  title="Collaboration Folder"
  description="Reserved for collaboration projects — received contributions will be stored here."
  kind="collaboration"
  getCommand="get_collaboration_dir"
  setCommand="set_collaboration_dir"
  clearCommand="clear_collaboration_dir"
  scanRoots={scanRoots}
  onRootsChanged={reloadRoots}
/>
```

(match the actual prop/callback names in FileManager.)

- [ ] **Step 3: Unconfigured hint**

In `src/contexts/TransfersContext.tsx`, in the existing `sync-finished` handler (the one that already raises the "frames arrived" notification), after a successful inbound outcome (`direction === 'received' && (outcome === 'ingested' || outcome === 'partial')`):

```tsx
const roots = await api.invoke<ScanRoot[]>('get_scan_roots');
if (!roots.some((r) => r.kind === 'sync_incoming')) {
  notify({
    title: 'Received files are landing in the app data folder',
    detail: 'Designate a Sync Incoming Folder in File Manager to keep them with your image library.',
    kind: 'sync',
    tone: 'warning',
    link: '/files',
    dedupeKey: 'sync-incoming-unconfigured',
  });
}
```

(`dedupeKey` keeps it to one hint; the dedupe set persists in localStorage per the notification conventions.)

- [ ] **Step 4: Typecheck + visual smoke**

Run: `npx tsc --noEmit`
Expected: clean. Then `npm run tauri dev`, open File Manager → both sections render, set/clear a folder works, second designation of the same kind shows the Conflict error.

- [ ] **Step 5: Commit**

```bash
git add src/components/SpecialFolderSection.tsx src/pages/FileManager.tsx src/contexts/TransfersContext.tsx src/types/models.ts
git commit -m "feat(ui): sync-incoming + collaboration folder sections in File Manager, unconfigured landing hint"
```

---

### Task 7: Perseus — multiple capture directories

**Files:**
- Modify: `crates/perseus/src/config.rs` (Config struct 227-244, `validate()` 273-353)
- Modify: `crates/perseus/src/run.rs` (Agent struct 192-201, `start_with_transport` 261-341, `shutdown` 373-388, `kill_for_test` 403-414, retention disk-probe 699-730)
- Modify: `crates/perseus/src/main.rs` (`cmd_status` banner), `crates/perseus/README.md`
- Test: config tests in `config.rs`'s test module; watcher smoke in `run.rs`/integration tests

**Interfaces:**
- Produces: `Config::capture_dirs_resolved(&self) -> Vec<PathBuf>` — every consumer of the old `config.capture_dir` goes through this.

- [ ] **Step 1: Failing config tests**

```rust
#[test]
fn capture_dirs_array_parses() {
    let c = Config::from_toml_str(&toml_with(r#"capture_dirs = ["/a", "/b"]"#)).unwrap();
    assert_eq!(c.capture_dirs_resolved(), vec![PathBuf::from("/a"), PathBuf::from("/b")]);
}
#[test]
fn capture_dir_singular_still_works() {
    let c = Config::from_toml_str(&toml_with(r#"capture_dir = "/a""#)).unwrap();
    assert_eq!(c.capture_dirs_resolved(), vec![PathBuf::from("/a")]);
}
#[test]
fn both_forms_rejected() {
    let cfg = Config::from_toml_str(&toml_with("capture_dir = \"/a\"\ncapture_dirs = [\"/b\"]")).unwrap();
    assert!(cfg.validate().unwrap_err().to_string().contains("either capture_dir or capture_dirs"));
}
#[test]
fn neither_form_rejected() {
    let cfg = Config::from_toml_str(&toml_with("")).unwrap();
    assert!(cfg.validate().is_err());
}
```

(`toml_with` = the file's existing minimal-valid-TOML test helper; add one if absent.)

- [ ] **Step 2: Run — expect FAIL**

Run: `cargo test -p perseus config`

- [ ] **Step 3: Implement**

Config struct:

```rust
    /// Single capture directory (legacy form). Exactly one of `capture_dir` /
    /// `capture_dirs` must be set.
    #[serde(default)]
    pub capture_dir: Option<PathBuf>,
    /// One or more capture directories to watch.
    #[serde(default)]
    pub capture_dirs: Vec<PathBuf>,
```

```rust
    /// The effective watch list: the array form, or the singular as a one-item
    /// list. `validate()` guarantees exactly one form is populated.
    pub fn capture_dirs_resolved(&self) -> Vec<PathBuf> {
        if let Some(d) = &self.capture_dir {
            vec![d.clone()]
        } else {
            self.capture_dirs.clone()
        }
    }
```

`validate()` replaces the single-dir block (274-283):

```rust
        match (&self.capture_dir, self.capture_dirs.is_empty()) {
            (Some(_), false) => anyhow::bail!(
                "set either capture_dir or capture_dirs in perseus.toml, not both"
            ),
            (None, true) => anyhow::bail!("capture_dir (or capture_dirs) is required"),
            _ => {}
        }
        for dir in self.capture_dirs_resolved() {
            if dir.as_os_str().is_empty() {
                anyhow::bail!("capture directory must not be empty");
            }
            if !dir.exists() {
                anyhow::bail!("capture directory does not exist: {}", dir.display());
            }
        }
```

`run.rs`:
- `watcher: Option<WatcherHandle>` → `watchers: Vec<WatcherHandle>`;
- spawn loop (replacing the single `spawn_watcher` call at ~line 308):

```rust
        let mut watchers = Vec::new();
        for dir in config.capture_dirs_resolved() {
            watchers.push(watcher::spawn_watcher(
                dir,
                config.stability(),
                config.poll_interval(),
                stable_tx.clone(),
                Arc::clone(&seen),
            )?);
        }
        drop(stable_tx); // consumer ends when the last watcher drops its sender
```

(`spawn_watcher` signature is unchanged — N watchers share one `mpsc::Sender` clone each.)
- `shutdown`: `for w in self.watchers { w.shutdown().await; }` (order before awaiting `enqueue_task`, as today); same for `kill_for_test` with `abort_for_test()`.
- Retention disk probe (712): probe **max usage across dirs** — `dirs.iter().map(probe_one).max().unwrap_or(0)` where `probe_one` is the existing statvfs closure body.
- `cmd_status` / startup banner: print the list (`capture_dirs = ["/a", "/b"]`).

- [ ] **Step 4: Multi-dir watcher smoke test**

In the run/integration test module (reuse the existing `Agent` test fixtures with loopback transport): two temp capture dirs, start agent, drop one eligible file in each, assert both get enqueued (2 packages / 2 files in `sync_outbound` via `status_snapshot` polling).

- [ ] **Step 5: Run + commit**

Run: `cargo test -p perseus`
Expected: PASS.

```bash
git add crates/perseus/ && git commit -m "feat(perseus): multiple capture directories (capture_dirs array, legacy capture_dir alias)"
```

---

### Task 8: Perseus — retention live-edit plumbing (`toml_edit` write-back + watch channel)

**Files:**
- Create: `crates/perseus/src/config_edit.rs`
- Modify: `crates/perseus/Cargo.toml` (`toml_edit = "0.23"`), `crates/perseus/src/lib.rs` (mod decl), `crates/perseus/src/run.rs` (`spawn_retention_task` 699-730, Agent holds the watch sender)
- Test: `config_edit.rs` unit tests

**Interfaces:**
- Produces:
  - `pub struct RetentionEdit { pub policy: RetentionPolicy, pub keep_days: u32, pub disk_max_pct: u8, pub interval_secs: u64, pub dry_run: bool }` (perseus `config::RetentionPolicy`)
  - `pub fn apply_retention_edit(config_path: &Path, edit: &RetentionEdit) -> anyhow::Result<Config>` — rewrites the file, returns the re-validated Config.
  - `Agent::retention_tx(&self) -> tokio::sync::watch::Sender<RetentionConfig>` (Task 9/10's web state consumes it).

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn retention_edit_preserves_comments_and_soak_keys() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("perseus.toml");
    std::fs::write(&p, r#"
# my precious comment
capture_dir = "/tmp"
data_dir = "/tmp"
mode = "auto"

[retention]
policy = "keep_everything"   # inline comment
dry_run = true
i_have_verified_the_soak = false
"#).unwrap();
    let edit = RetentionEdit { policy: RetentionPolicy::KeepDays, keep_days: 14, disk_max_pct: 90, interval_secs: 1800, dry_run: true };
    let cfg = apply_retention_edit(&p, &edit).unwrap();
    assert_eq!(cfg.retention.keep_days, 14);
    let text = std::fs::read_to_string(&p).unwrap();
    assert!(text.contains("# my precious comment"));
    assert!(text.contains("i_have_verified_the_soak = false"), "soak key untouched");
    assert!(text.contains("keep_days = 14"));
}

#[test]
fn retention_edit_cannot_enable_live_deletion() {
    // dry_run=false while the file has i_have_verified_the_soak=false must be
    // rejected by the re-validate step (Config::validate's two-key gate).
    // Assert the file on disk is UNCHANGED after the failed edit.
}
```

- [ ] **Step 2: Run — FAIL (module missing)**

Run: `cargo test -p perseus config_edit`

- [ ] **Step 3: Implement `config_edit.rs`**

```rust
use std::path::Path;
use anyhow::{Context, Result};
use crate::config::{Config, RetentionPolicy};

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionEdit {
    pub policy: RetentionPolicy,
    pub keep_days: u32,
    pub disk_max_pct: u8,
    pub interval_secs: u64,
    pub dry_run: bool,
}

/// Rewrite ONLY the `[retention]` keys the web UI may edit, preserving all
/// comments/layout (`toml_edit`), then re-parse + validate the whole file.
/// The two live-deletion keys are deliberately not writable here (spec §4).
/// On any error the file is left untouched (edit happens on a copy, written
/// via tmp + atomic rename only after validation passes).
pub fn apply_retention_edit(config_path: &Path, edit: &RetentionEdit) -> Result<Config> {
    let original = std::fs::read_to_string(config_path)
        .with_context(|| format!("read {}", config_path.display()))?;
    let mut doc: toml_edit::DocumentMut = original.parse().context("parse perseus.toml")?;
    let table = doc["retention"].or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
    table["policy"] = toml_edit::value(policy_str(&edit.policy));
    table["keep_days"] = toml_edit::value(edit.keep_days as i64);
    table["disk_max_pct"] = toml_edit::value(edit.disk_max_pct as i64);
    table["interval_secs"] = toml_edit::value(edit.interval_secs as i64);
    table["dry_run"] = toml_edit::value(edit.dry_run);

    let candidate = doc.to_string();
    let cfg = Config::from_toml_str(&candidate).context("re-parse edited config")?;
    cfg.validate().context("edited config failed validation")?;

    let tmp = config_path.with_extension("toml.tmp");
    std::fs::write(&tmp, &candidate).context("write tmp config")?;
    std::fs::rename(&tmp, config_path).context("replace config")?;
    tracing::info!(policy = policy_str(&edit.policy), dry_run = edit.dry_run, "retention config edited via web");
    Ok(cfg)
}

fn policy_str(p: &RetentionPolicy) -> &'static str {
    match p {
        RetentionPolicy::KeepEverything => "keep_everything",
        RetentionPolicy::OnConfirm => "on_confirm",
        RetentionPolicy::KeepDays => "keep_days",
        RetentionPolicy::DiskPct => "disk_pct",
    }
}
```

(Match the actual `RetentionPolicy` variant shapes in `config.rs:96-107` — if variants carry no payload there, the above holds; `or_insert` API name per toml_edit 0.23 — `doc["retention"].or_insert(...)` is `Item::or_insert`; if the exact method differs, use `doc.entry("retention").or_insert(...)`.)

- [ ] **Step 4: Watch channel in run.rs**

`Agent` gains `retention_tx: tokio::sync::watch::Sender<crate::config::RetentionConfig>` (+ accessor `pub fn retention_tx(&self) -> watch::Sender<RetentionConfig>` returning a clone). `start_with_transport` creates `let (retention_tx, retention_rx) = tokio::sync::watch::channel(config.retention.clone());` and `spawn_retention_task` takes `retention_rx`:

```rust
    tokio::spawn(async move {
        let mut rx = retention_rx;
        loop {
            let cfg = rx.borrow().clone();
            let interval = std::time::Duration::from_secs(cfg.interval_secs);
            tokio::select! {
                _ = tokio::time::sleep(interval) => { /* run one pass with cfg */ }
                changed = rx.changed() => {
                    if changed.is_err() { break; }        // sender dropped = shutdown
                    tracing::info!("retention config updated; applying on next pass");
                    continue;                              // re-borrow the new cfg
                }
            }
            // existing spawn_blocking(run_retention_once(...)) body, using `cfg`
        }
    });
```

(Keep the existing pass body — only the config source changes from the captured `config.clone()` to `rx.borrow().clone()` per iteration.)

- [ ] **Step 5: Run + commit**

Run: `cargo test -p perseus`

```bash
git add crates/perseus/ && git commit -m "feat(perseus): toml_edit retention write-back + watch-channel live apply"
```

---

### Task 9: Perseus web server — skeleton, auth, read endpoints

**Files:**
- Create: `crates/perseus/src/web.rs`, `crates/perseus/src/web/index.html`
- Modify: `crates/perseus/Cargo.toml` (`axum = "0.8"`, `tower = { version = "0.5", features = ["util"] }` as dev-dep for oneshot tests), `crates/perseus/src/lib.rs`, `crates/perseus/src/config.rs` (web fields), `crates/perseus/src/run.rs` (spawn server), `crates/perseus/src/main.rs`
- Test: `web.rs` handler tests (`tower::ServiceExt::oneshot`)

**Interfaces:**
- Produces: `pub fn build_router(state: Arc<WebState>) -> axum::Router`; `pub struct WebState { pub store: Arc<StandaloneSyncStore>, pub engine: Arc<SyncEngineHandle>, pub config_path: PathBuf, pub config: tokio::sync::RwLock<Config>, pub retention_tx: watch::Sender<RetentionConfig>, pub retention_log: Arc<Mutex<VecDeque<RetentionRunRecord>>>, pub device_names: HashMap<String, String>, pub capture_dirs: Vec<PathBuf> }` — Task 10 adds write handlers onto the same router.
- Consumes: `Agent` handles (Task 7/8), `SyncStore` trait methods, `search_history_rows`.

- [ ] **Step 1: Config fields + validation test**

`config.rs`:

```rust
    /// Local web status page bind address. Empty string disables the server.
    #[serde(default = "default_web_bind")]
    pub web_bind: String,
    /// Bearer token required when web_bind is not a loopback address.
    #[serde(default)]
    pub web_token: Option<String>,
```
```rust
fn default_web_bind() -> String { "127.0.0.1:8686".to_string() }
```

`validate()` addition:

```rust
        if !self.web_bind.is_empty() {
            let addr: std::net::SocketAddr = self.web_bind.parse().map_err(|e| {
                anyhow::anyhow!("web_bind is not a valid socket address ({}): {e}", self.web_bind)
            })?;
            if !addr.ip().is_loopback() && self.web_token.as_deref().unwrap_or("").is_empty() {
                anyhow::bail!(
                    "web_bind {} is not loopback — set web_token to protect the status page",
                    self.web_bind
                );
            }
        }
```

Tests: default parses to `127.0.0.1:8686`; `web_bind = "0.0.0.0:8686"` without token → validate error; with token → Ok; `web_bind = ""` → Ok (disabled).

- [ ] **Step 2: Failing router tests**

In `web.rs`'s test module (fixture: temp `StandaloneSyncStore` seeded with one confirmed + one transferring `sync_outbound` row and two `sync_history` rows via the store's own API):

```rust
#[tokio::test]
async fn status_endpoint_shape() {
    let (state, _tmp) = test_state().await;
    let app = build_router(state, None);
    let res = app
        .oneshot(Request::builder().uri("/api/status").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v: serde_json::Value = body_json(res).await;
    assert!(v["captureDirs"].is_array());
    assert!(v["retention"]["policy"].is_string());
    assert!(v["inFlight"].is_array());
}

#[tokio::test]
async fn bearer_required_when_token_set() {
    let (state, _tmp) = test_state().await;
    let app = build_router(state, Some("s3cret".to_string()));
    let unauth = app.clone()
        .oneshot(Request::builder().uri("/api/status").body(Body::empty()).unwrap())
        .await.unwrap();
    assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);
    let auth = app
        .oneshot(Request::builder().uri("/api/status")
            .header("authorization", "Bearer s3cret")
            .body(Body::empty()).unwrap())
        .await.unwrap();
    assert_eq!(auth.status(), StatusCode::OK);
}

#[tokio::test]
async fn sent_and_history_endpoints() {
    // /api/sent → both rows with state strings; /api/sent?state=confirmed → 1;
    // /api/history?query=frame → filtered rows with durationSecs + peerName fields.
}
```

- [ ] **Step 3: Implement `web.rs`**

```rust
pub struct WebState { /* as in Interfaces above */ }

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusDto {
    capture_dirs: Vec<String>,
    in_flight: Vec<SentDto>,
    retention: RetentionDto,
    counts: CountsDto, // confirmed_total, failed_total, queued — from store queries
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SentDto {
    id: i64,
    package_ref: String,
    state: String,
    attempts: u32,
    created_at: String,
    confirmed_at: Option<String>,
    deletable: bool, // state == "confirmed" — the single safe-to-delete predicate surfaced
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryDto {
    filename: String,
    object: Option<String>,
    peer_device: String,
    peer_name: Option<String>,    // device_names lookup, None when unknown
    direction: String,
    bytes: u64,
    started_at: String,
    finished_at: Option<String>,
    duration_secs: Option<f64>,   // finished - started when both parse (chrono)
    outcome: String,
}

pub fn build_router(state: Arc<WebState>, token: Option<String>) -> Router {
    // `token` is snapshotted from Config.web_token at spawn time (auth config
    // changes require a restart — deliberate, keeps the middleware trivial).
    Router::new()
        .route("/", get(index_html))
        .route("/api/status", get(api_status))
        .route("/api/sent", get(api_sent))
        .route("/api/history", get(api_history))
        .with_state(state)
        .layer(axum::middleware::from_fn(move |req, next| {
            auth_layer(token.clone(), req, next)
        }))
}
```

- `index_html`: `Html(include_str!("web/index.html"))` (page content in Task 10).
- `api_sent`: query param `state` optional; reads all outbound rows — add the missing store accessor as a free fn in `crates/athenaeum-core/src/sync/store.rs` next to `confirmed_outbound_rows` (line 279):

```rust
/// Every outbound row, newest first, capped. The web status page's "sent" list.
pub fn all_outbound_rows(conn: &Connection, limit: u32) -> Result<Vec<OutboundRow>> {
    // same row mapping as confirmed_outbound_rows, without the WHERE clause,
    // ORDER BY id DESC LIMIT ?1
}
```
plus a `StandaloneSyncStore::all_outbound(limit)` inherent method delegating to it (and a `CatalogSyncStore` twin for symmetry).
- `api_history`: `HistoryQuery { filename: q, object: q, direction, peer: None, limit }` — reuse `search_history` then map to `HistoryDto` (duration via `chrono::DateTime::parse_from_rfc3339` on both stamps).
- `auth_layer`: when token is `Some`, compare `Authorization: Bearer <t>`; wrong/missing → `StatusCode::UNAUTHORIZED`. When `None` (loopback bind), pass through.
- Errors: every handler returns `Result<Json<T>, (StatusCode, String)>` and `tracing::error!`s before returning (never-swallow).

`run.rs`: after the retention task spawn, when `!config.web_bind.is_empty()`:

```rust
        let listener = tokio::net::TcpListener::bind(&config.web_bind)
            .await
            .with_context(|| format!("bind web status page {}", config.web_bind))?;
        tracing::info!(bind = %config.web_bind, "web status page online");
        let router = web::build_router(web_state);
        web_task = Some(tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, router).await {
                tracing::error!(error = %e, "web status page server exited");
            }
        }));
```

(`web_task: Option<JoinHandle<()>>` on Agent, aborted in `shutdown`.) `device_names` in `WebState` comes from Task 11's cache — until that task lands, construct with `HashMap::new()`.

- [ ] **Step 4: Run + commit**

Run: `cargo test -p perseus web && cargo test -p perseus`
Expected: PASS.

```bash
git add crates/perseus/ crates/athenaeum-core/src/sync/store.rs
git commit -m "feat(perseus): embedded web status page — status/sent/history endpoints + bearer auth"
```

---

### Task 10: Perseus web — retention log, policy GET/PUT, manual delete, HTML page

**Files:**
- Modify: `crates/perseus/src/web.rs`, `crates/perseus/src/run.rs` (retention task records outcomes; extract the delete closure), `crates/perseus/src/web/index.html` (full page)
- Test: `web.rs` handler tests

**Interfaces:**
- Produces: `pub struct RetentionRunRecord { pub at: String, pub dry_run: bool, pub policy: String, pub deleted: Vec<String>, pub would_delete: Vec<String>, pub errors: Vec<String> }` (ring buffer, cap 50); `pub fn delete_confirmed_packages(store: &StandaloneSyncStore, seen: &SeenStore, ids: &[i64]) -> anyhow::Result<DeleteReport>` in `run.rs` (shared by the endpoint; the same source-deletion body the retention deleter closure uses).
- Consumes: `config_edit::apply_retention_edit` + `retention_tx` (Task 8).

- [ ] **Step 1: Failing tests**

```rust
#[tokio::test]
async fn delete_rejects_non_confirmed() {
    // seed: row A state='confirmed' with a real temp source file registered in
    // sync_sources; row B state='transferring'.
    // POST /api/delete {"ids":[A,B]} → 200 with {deleted:[A], rejected:[{id:B, reason:"not confirmed"}]};
    // A's source file gone from disk, B's untouched; sync_history gained one
    // 'deleted_manual' row for A.
}

#[tokio::test]
async fn retention_policy_roundtrip() {
    // GET /api/retention/policy → current values + readOnly:{soakOptIn,liveDeletion}.
    // PUT valid edit → 200; watch receiver sees the new value; file rewritten.
    // PUT with dry_run=false (soak not verified in file) → 422, file unchanged.
}

#[tokio::test]
async fn retention_log_returns_ring_buffer() {
    // push two RetentionRunRecords into state.retention_log; GET /api/retention/log → 2 entries, newest first.
}
```

- [ ] **Step 2: Run — FAIL**

Run: `cargo test -p perseus web`

- [ ] **Step 3: Implement**

1. **Retention records:** in the Task 8 retention loop, wrap each pass's outcome: map the `RetentionOutcome` returned by the pass (its deleted / would-delete path lists — see `crates/athenaeum-core/src/sync/retention.rs` `RetentionOutcome`, ~line 155-170, map its `Vec` fields verbatim into the record) into `RetentionRunRecord` and push-front into `state.retention_log` (`VecDeque`, `truncate(50)`).
2. **`GET /api/retention/log`:** serialize the deque.
3. **`GET /api/retention/policy`:**
```rust
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PolicyDto {
    policy: String, keep_days: u32, disk_max_pct: u8, interval_secs: u64, dry_run: bool,
    // read-only visibility of the two-key gate (never writable here):
    soak_opt_in: bool, live_deletion_possible: bool,
}
```
(field sources: `config.read().await.retention` + `i_have_verified_the_soak`; name the DTO fields to match what `config.rs` actually calls them.)
4. **`PUT /api/retention/policy`** body = `RetentionEdit` (Task 8's serde type): call `apply_retention_edit(&state.config_path, &edit)`; on Ok update `*state.config.write().await = new_cfg.clone()` and `state.retention_tx.send(new_cfg.retention.clone())`; on validation error → `(StatusCode::UNPROCESSABLE_ENTITY, msg)`.
5. **`POST /api/delete`** body `{ ids: Vec<i64> }`: `delete_confirmed_packages` in `run.rs` — extract the existing retention deleter closure body (the perseus-side source delete: `live_sources_for_package` → `std::fs::remove_file` → `mark_sync_source_deleted` → `insert_history_row` with outcome `"deleted_manual"` here vs retention's `"retention_deleted"`) into the shared fn; the endpoint verifies each id's row `state == "confirmed"` first (reject otherwise, per-id reasons in the response). This is the same confirmed()-only chokepoint — deletion is impossible for anything else by construction.
6. **`index.html`** — one self-contained page (no external assets; CSP-friendly inline CSS/JS), dark theme, four sections: **Status** (capture dirs, counts, in-flight table), **Sent** (table: package, state chip, created, confirmed, per-row Delete button enabled only when `deletable`, checkbox multi-select + Delete selected), **History** (search box → `/api/history?query=`, columns: file, object, peer name (fallback short hex), direction, size, duration, outcome; confirmed sends get a green `✓ safe to delete` chip), **Retention** (policy form bound to GET/PUT, dry-run toggle, read-only soak/live indicators with hint text "Live deletion is enabled in perseus.toml only", log tail list). Poll `/api/status` + `/api/sent` every 2 s, history on demand. Keep it ~200 lines, vanilla JS `fetch` with the bearer token read from `localStorage.perseusToken` (settable via a prompt when a 401 comes back).

- [ ] **Step 4: Run + manual smoke**

Run: `cargo test -p perseus`
Then: `cargo run -p perseus -- --config <test-config> run` → open `http://127.0.0.1:8686` → page renders, tables fill, retention form saves and the TOML file shows the change with comments intact.

- [ ] **Step 5: Commit**

```bash
git add crates/perseus/
git commit -m "feat(perseus): web page — retention log + live policy editing + manual delete of confirmed packages"
```

---

### Task 11: Device names — app command + Perseus cache

**Files:**
- Modify: `crates/athenaeum-core/src/api/sync.rs` (new fn), `crates/athenaeum-tauri/src/commands/sync.rs`, `crates/athenaeum-tauri/src/lib.rs`, `crates/athenaeum-web/src/routes/sync.rs`, `crates/athenaeum-web/src/routes/mod.rs`
- Modify: `crates/perseus/src/account.rs` (`PairingCache` 42-86, populate sites at 177-180 and 311), `crates/perseus/src/web.rs` (WebState.device_names from cache)
- Test: core api test + perseus cache round-trip tests

**Interfaces:**
- Produces: core `pub async fn get_sync_device_names(ctx: &ServiceContext) -> Result<std::collections::HashMap<String, String>, ApiError>` — key = 64-char lowercase hex node id, value = hub device name. Command name: `get_sync_device_names` (Task 12 consumes from the frontend).
- Consumes: `api::account::list_devices(ctx)` (exists, api/account.rs:341), `sync::pairing::node_id_from_pubkey_b64` (pairing.rs:311).

- [ ] **Step 1: Failing perseus cache test**

```rust
#[test]
fn pairing_cache_device_names_roundtrip_and_backcompat() {
    // (a) old-format JSON without device_names parses (serde default → empty map);
    // (b) save with names → load returns them.
    let old = r#"{"device_id":"d","primary_device_id":"p","peer_node_id_hex":"ab","relay_urls":[]}"#;
    let c: PairingCache = serde_json::from_str(old).unwrap();
    assert!(c.device_names.is_empty());
}
```

- [ ] **Step 2: Implement**

Perseus `PairingCache` gains:

```rust
    /// node_id_hex → hub device name, refreshed whenever the device list is
    /// fetched. Display-only (history rows keep the hex as the stable key).
    #[serde(default)]
    pub device_names: std::collections::HashMap<String, String>,
```

Populate at both `list_devices` call sites (login 177-180, `build_account_pairing` 311): for each device, `node_id_from_pubkey_b64(&d.pubkey)` → hex via the existing hex helper → `names.insert(hex, d.name.clone())`; store into the cache before `save()`. `WebState.device_names` loads from `PairingCache::load(...)` at agent start; `HistoryDto.peer_name = state.device_names.get(&row.peer_device).cloned()`.

Core api fn in `api/sync.rs`:

```rust
/// Map of node-id-hex → hub device name for history display. Best-effort:
/// hub unreachable or signed out → empty map (UI falls back to short hex).
pub async fn get_sync_device_names(
    ctx: &ServiceContext,
) -> Result<std::collections::HashMap<String, String>, ApiError> {
    let devices = match crate::api::account::list_devices(ctx).await {
        Ok(d) => d,
        Err(e) => {
            tracing::debug!(error = %format!("{e:?}"), "device names unavailable; falling back to hex");
            return Ok(Default::default());
        }
    };
    let mut map = std::collections::HashMap::new();
    for d in devices {
        if let Ok(id) = crate::sync::pairing::node_id_from_pubkey_b64(&d.pubkey) {
            map.insert(hex32(&id), d.name.clone());
        }
    }
    Ok(map)
}
```

(reuse/move the receiver's `hex32` helper — receiver.rs:256 — into a shared spot, e.g. `sync/mod.rs`, updating its one caller.) Tauri command + web route + registrations, `#[tracing::instrument(skip_all, err)]`, per the Task 4 wrapper pattern. Return type is a plain map — no ts_export entry needed.

- [ ] **Step 3: Run + commit**

Run: `cargo test -p perseus && cargo test -p athenaeum-core && cargo build --workspace 2>&1 | tail -3`

```bash
git add crates/ && git commit -m "feat(sync): device-name resolution for history — app command + perseus pairing-cache names"
```

---

### Task 12: TransfersPanel polish (device names, duration, safe-to-delete badge)

**Files:**
- Modify: `src/components/transfers/TransfersPanel.tsx`
- Test: `npx tsc --noEmit` + visual check

**Interfaces:**
- Consumes: `api.invoke<Record<string, string>>('get_sync_device_names')`; existing `HistoryRow` fields (`startedAt`, `finishedAt`, `bytes`, `peerDevice`, `outcome`, `direction`).

- [ ] **Step 1: Implement**

1. On panel open (the effect that loads history), also load names once: `const names = await api.invoke<Record<string, string>>('get_sync_device_names')` into state; render peer cell as `names[r.peerDevice] ?? shortPeer(r.peerDevice)` (same in the Active tab via `OutboundSummary.peerShort` — active rows keep `peerShort`, names apply where the full hex is available, i.e. history).
2. Duration + speed helper in the file:

```tsx
function transferStats(r: HistoryRow): string | null {
  if (!r.finishedAt) return null;
  const ms = new Date(r.finishedAt).getTime() - new Date(r.startedAt).getTime();
  if (!isFinite(ms) || ms < 0) return null;
  const secs = ms / 1000;
  const dur = secs >= 60 ? `${Math.floor(secs / 60)}m ${Math.round(secs % 60)}s` : `${secs.toFixed(1)}s`;
  const mbs = r.bytes > 0 && secs > 0 ? ` · ${(r.bytes / 1048576 / secs).toFixed(1)} MB/s` : '';
  return `${dur}${mbs}`;
}
```
   Render it as a muted suffix line under the timestamp (`text-content-muted`).
3. Badge: for `r.direction === 'sent' && r.outcome === 'sent'` rows (sender-confirmed history — written only by `append_confirmed_history`), render a small chip: `<span className="text-xs text-success">✓ delivered — safe to delete</span>` (use the repo's success token; if there's no `text-success`, use the token the scan-finished toast uses). Received rows show their existing outcome text unchanged.

- [ ] **Step 2: Gate + visual smoke**

Run: `npx tsc --noEmit`
Then `npm run tauri dev` → Transfers panel → history shows names (signed in), durations, badges; signed-out shows short hex (empty names map).

- [ ] **Step 3: Commit**

```bash
git add src/components/transfers/
git commit -m "feat(ui): transfers history polish — device names, duration/speed, safe-to-delete badge"
```

---

### Task 13: Final gates, docs, release-notes hygiene

**Files:**
- Modify: `crates/perseus/README.md` (multi-dir config, `[web]` fields, web page usage + token note, manual delete semantics)
- Modify: `CLAUDE.md` — one line in the Stage-I sync area if present (landing root + web page existence); skip if no sync section exists yet
- No uninstall-script changes: no new data locations (blobs/config are pre-existing paths)

- [ ] **Step 1: Full gate run**

```bash
cargo build --workspace --all-targets 2>&1 | tail -3
cargo test -p athenaeum-core 2>&1 | tail -3
cargo test -p perseus 2>&1 | tail -3
cargo check -p athenaeum-core --no-default-features 2>&1 | tail -2
npx tsc --noEmit
```
Expected: all green (`~650+` core tests, perseus suite, clean tsc).

- [ ] **Step 2: README + docs**

Perseus README: `capture_dirs` example, web page section (default bind, token requirement for non-loopback, what each tab shows, "delete is possible only for confirmed packages", "live deletion keys remain TOML-only"). Spec cross-link.

- [ ] **Step 3: Live verification on the real deployment (owner-visible)**

On the mini (primary) and the observatory box: build, restart Perseus, confirm in logs: `blob store startup sweep removed stale tags count=N` (N > 0 expected on both — the accumulated leak), then one real package cycle → `blobs released` on both sides; `du -sh <data_dir>/sync_blobs` stops growing. Open the web page, verify status/sent/history, edit retention (dry-run stays on), delete one confirmed test package by click.

- [ ] **Step 4: Commit**

```bash
git add crates/perseus/README.md CLAUDE.md
git commit -m "docs(perseus): multi-dir + web status page documentation; stage 1.5 wrap-up"
```

---

## Plan self-review notes (kept for the executor)

- **Spec coverage:** §2 → Tasks 4-6; §3 → Tasks 1-3; §4 → Tasks 7-10; §5 → Tasks 11-12; §6 test matrix distributed into each task's steps; §7 ordering = task order (GC first — the leak is live).
- **Spec deviation (documented):** §6's "extend `sync_e2e.rs` with blob-store-empty assertions" — the e2e harness is loopback-based and has no blob store; the assertion lives in `engine_suite_over_iroh` (Task 3 Step 5) instead, which runs the same engine suite over real iroh stores. The e2e harness instead gains the landing-root assertion (Task 5).
- **Interface consistency:** `release(&PackageId)` (Tasks 1-3), `package_tag` (1-2), `capture_dirs_resolved` (7, 9), `RetentionEdit`/`apply_retention_edit` (8, 10), `retention_tx` (8, 9, 10), `get_sync_device_names` (11, 12), `IncomingResolver` (5).
- **Known soft spots the executor must resolve against real code (all with exact pointers):** iroh-blobs `Options` import path (Task 1 Step 4b); `Pending.announce` optionality in `fail_package`/`cancel_package` (Task 3 Step 3); `RetentionOutcome` field names (Task 10 Step 3.1); calibration `clear_*` semantics to mirror (Task 4 Step 3.3); `wait_until` signature variants in iroh tests (Task 3 Step 5).

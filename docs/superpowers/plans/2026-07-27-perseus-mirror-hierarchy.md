# Perseus Mirror Hierarchy Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Perseus setting (`mirror_hierarchy`) that makes every Perseus send land on the receiver in ONE stable tree mirroring the capture hierarchy — `<incoming>/<sender_slug>/<rel_path>` — instead of per-batch folders.

**Architecture:** The flag is stamped per transfer at enqueue (`sync_outbound.layout`), travels as an appended `Msg::Announce4` wire variant (emitted ONLY for mirror; batch keeps `Announce3`), and the receiver realizes mirror by skipping the batch-slug `landing_override` — falling into the already-tested v1 landing path (`<incoming>/<sender_slug>/<rel_path>`, per-file `unique_path` collision suffixing). Spec: `docs/superpowers/specs/2026-07-27-perseus-mirror-hierarchy-design.md`.

**Tech Stack:** Rust (athenaeum-core `sync`/`sharing`, perseus crate), postcard wire enum with golden pins, rusqlite guarded-ALTER migrations, vanilla-JS Perseus web UI.

## Global Constraints

- Branch: create `0.5.2` off the current `0.5.1` HEAD; commit as `eg013ra1n <vilen.sharifov@gmail.com>` (never Claude as author/co-author). Push only to `origin` (GitLab).
- **NEVER stage the parallel session's churn**: `rustafits` (submodule pointer), `Cargo.lock`, `.gitlab-ci.yml`, `docker/Dockerfile`, `crates/jpeg-encode/*`, `crates/athenaeum-core/Cargo.toml`. Always `git add` explicit paths, never `-A`/`-u`.
- Wire enum is APPEND-ONLY: new `Msg` variants go LAST; never reorder/retype/remove; existing golden-pin literals in `sharing/wire_golden_tests.rs` must NEVER be edited — only a NEW pin may be added (its hex is captured from the first encode of the NEW type).
- Logging: `tracing` only, message = short stable phrase + snake_case fields; no `println!` outside the exempt CLI paths.
- Gates per task: `cargo test -p athenaeum-core --lib` and/or `cargo test -p perseus` as listed; final task runs both + `cargo build -p athenaeum-web`. NOTE: a clean-checkout workspace build with default features is blocked on the rustafits submodule pointer (known 0.5.1 merge gate, not this plan's problem) — the dirty working tree builds fine; do not chase it.
- No new Tauri command / Axum route is added (the setting is Perseus-side; desktop backends only see a threaded parameter), so the two-backend rule is not triggered.

---

### Task 1: `PackageLayout` type + `sync_outbound.layout` column

**Files:**
- Modify: `crates/athenaeum-core/src/sharing/types.rs` (add enum after `AnnounceFileEntry`, ~line 42)
- Modify: `crates/athenaeum-core/src/sync/store.rs` (`DDL_OUTBOUND` ~line 34, `ensure_outbound_columns` ~line 63)
- Modify: `crates/athenaeum-core/src/sync/models.rs` (`OutboundRow` ~line 315, its row-mapping fns in `store.rs` — `to_outbound` ~line 701, `outbound_raw_from_row` ~line 789: add `layout` to the SELECT column lists and struct literals)
- Test: inline `#[cfg(test)]` in `sharing/types.rs` + existing store test module in `sync/store.rs`

**Interfaces:**
- Produces: `PackageLayout { Batch, Mirror }` with `as_str() -> &'static str` (`"batch"`/`"mirror"`), `from_db(&str) -> PackageLayout` (unknown → `Batch` + `tracing::warn!`), `Default = Batch`; `OutboundRow.layout: PackageLayout`.
- Later tasks rely on: the enum living in `sharing::types` (wire + DB + perseus all import it), the DB TEXT repr `'batch'|'mirror'`, column default `'batch'`.

- [ ] **Step 1: Write the failing tests**

In `sharing/types.rs` test module (create one if the file has none):

```rust
#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn package_layout_db_roundtrip_and_default() {
        assert_eq!(PackageLayout::Batch.as_str(), "batch");
        assert_eq!(PackageLayout::Mirror.as_str(), "mirror");
        assert_eq!(PackageLayout::from_db("mirror"), PackageLayout::Mirror);
        assert_eq!(PackageLayout::from_db("batch"), PackageLayout::Batch);
        // Unknown value degrades to Batch (never a parse failure on read).
        assert_eq!(PackageLayout::from_db("wat"), PackageLayout::Batch);
        assert_eq!(PackageLayout::default(), PackageLayout::Batch);
    }
}
```

In `sync/store.rs` tests (next to the existing `ensure_outbound_columns`-era tests):

```rust
    #[test]
    fn outbound_layout_column_exists_and_defaults_to_batch() {
        let tmp = tempfile::tempdir().unwrap();
        let store = StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap();
        let conn = store.lock_conn();
        // Insert via the current signature (layout not yet a parameter) — the
        // column default must back-fill 'batch'.
        let id = insert_outbound_with_files(&conn, "/tmp/pkg-x", &"aa".repeat(32), None, &[])
            .unwrap();
        let row = get_outbound_row(&conn, id).unwrap().unwrap();
        assert_eq!(row.layout, PackageLayout::Batch);
    }
```

(Adapt the open/read helper names to the ones the neighboring tests in `store.rs` actually use — e.g. if reads go through `to_outbound`, fetch with that path. The assertion is the deliverable: a fresh row reads `layout == Batch`.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p athenaeum-core --lib package_layout_db_roundtrip layout_column_exists`
Expected: FAIL — `PackageLayout` not found / no `layout` field.

- [ ] **Step 3: Implement**

`sharing/types.rs` (after `AnnounceFileEntry`):

```rust
/// How the receiver lays a transfer's files on disk (spec 2026-07-27
/// perseus-mirror-hierarchy). `Batch` = today's per-transfer
/// `<sender_slug>/<batch_slug>/` folder; `Mirror` = the stable
/// `<sender_slug>/<rel_path>` capture-mirror tree (no batch level). Postcard
/// variant order is FROZEN (this rides `Msg::Announce4`); append-only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PackageLayout {
    #[default]
    Batch,
    Mirror,
}

impl PackageLayout {
    pub fn as_str(self) -> &'static str {
        match self {
            PackageLayout::Batch => "batch",
            PackageLayout::Mirror => "mirror",
        }
    }

    /// DB TEXT → layout. Unknown text degrades to `Batch` (a read never fails
    /// on a value written by a newer build).
    pub fn from_db(s: &str) -> Self {
        match s {
            "mirror" => PackageLayout::Mirror,
            "batch" => PackageLayout::Batch,
            other => {
                tracing::warn!(value = other, "unknown sync_outbound.layout; defaulting to batch");
                PackageLayout::Batch
            }
        }
    }
}
```

`sync/store.rs` — `DDL_OUTBOUND`: add `layout TEXT NOT NULL DEFAULT 'batch'` as the last column. `ensure_outbound_columns`: append to the guarded array:

```rust
        // Perseus mirror-hierarchy (spec 2026-07-27): receiver landing layout,
        // stamped per transfer at enqueue. Constant default back-fills 'batch'.
        ("layout", "TEXT NOT NULL DEFAULT 'batch'"),
```

`sync/models.rs` — `OutboundRow`: add

```rust
    #[serde(default)]
    pub layout: PackageLayout,
```

`sync/store.rs` — extend the outbound SELECT column lists and the two row-mapping constructors (`to_outbound`, `outbound_raw_from_row`) with `layout` read as `String` → `PackageLayout::from_db(&s)`. Follow exactly how `generation` was threaded (same fns, same style).

- [ ] **Step 4: Run tests**

Run: `cargo test -p athenaeum-core --lib package_layout layout_column outbound`
Expected: PASS, including every pre-existing outbound store test.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/sharing/types.rs crates/athenaeum-core/src/sync/store.rs crates/athenaeum-core/src/sync/models.rs
git commit -m "feat(sync): PackageLayout type + sync_outbound.layout column (mirror-hierarchy T1)"
```

---

### Task 2: stamp layout at enqueue (core signature threading)

**Files:**
- Modify: `crates/athenaeum-core/src/sync/engine.rs` (`enqueue_package` ~line 673, `Command::Process`, the worker arm that calls `SyncStore::enqueue`)
- Modify: `crates/athenaeum-core/src/sync/store.rs` (`SyncStore::enqueue` trait ~line 550 + both impls ~lines 2652/2988, `insert_outbound_with_files` ~line 2485)
- Modify: `crates/athenaeum-core/src/api/sync.rs` (every `enqueue_package` caller; `resend_declined_as_new_transfer` ~line 3505 passes `row.layout`)
- Modify: `crates/perseus/src/run.rs` (`enqueue_package_to_all` ~line 1481 — add a `layout: PackageLayout` param, pass through), `crates/perseus/src/batcher.rs` (`deliver_batch` ~line 373 — pass `PackageLayout::Batch` placeholder for now), `crates/perseus/src/resend.rs` (`send_batch_to_target` ~line 731 — placeholder `PackageLayout::Batch`)
- Test: store test in `sync/store.rs`; engine/loopback test if one already enqueues (extend, don't invent a harness)

**Interfaces:**
- Consumes: `PackageLayout` from Task 1.
- Produces: `SyncEngineHandle::enqueue_package(dir, display_name, files, layout: PackageLayout) -> Result<i64>`; `SyncStore::enqueue(package_ref, peer, display_name, files, layout)`; `insert_outbound_with_files(conn, package_ref, peer_hex, display_name, files, layout)`. Perseus `enqueue_package_to_all(engines, pkg_dir, display_name, files, layout)`. Task 6 replaces the Perseus `Batch` placeholders with real config values; Task 3 reads `row.layout` at announce time.

- [ ] **Step 1: Write the failing test**

`sync/store.rs` tests:

```rust
    #[test]
    fn enqueue_stamps_layout_on_the_row() {
        let tmp = tempfile::tempdir().unwrap();
        let store = StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap();
        let conn = store.lock_conn();
        let id = insert_outbound_with_files(
            &conn, "/tmp/pkg-m", &"aa".repeat(32), Some("Night"), &[], PackageLayout::Mirror,
        )
        .unwrap();
        let row = get_outbound_row(&conn, id).unwrap().unwrap();
        assert_eq!(row.layout, PackageLayout::Mirror);
        // A resend reset (targeted UPDATE) must preserve the stamp.
        reset_outbound_for_resend(&conn, id, "new-wire-id").unwrap();
        let row = get_outbound_row(&conn, id).unwrap().unwrap();
        assert_eq!(row.layout, PackageLayout::Mirror, "resend reset preserves layout");
    }
```

(Adapt helper names to the module's existing test vocabulary; `reset_outbound_for_resend`'s real signature is at store.rs:1036 — call it the way its own tests do.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p athenaeum-core --lib enqueue_stamps_layout`
Expected: FAIL — `insert_outbound_with_files` takes 5 args.

- [ ] **Step 3: Implement — compiler-driven threading**

1. `insert_outbound_with_files`: add `layout: PackageLayout` last param; INSERT becomes
   `INSERT INTO sync_outbound (package_ref, peer, state, attempts, created_at, display_name, layout) VALUES (?1, ?2, ?3, 0, ?4, ?5, ?6)` with `layout.as_str()`.
2. `SyncStore::enqueue` trait + both impls: add `layout: PackageLayout`, delegate.
3. `engine.rs`: `Command::Process { dir, display_name, files, layout, reply }`; `enqueue_package(&self, dir, display_name, files, layout)` forwards it; the worker arm passes it to `store.enqueue(...)`.
4. Fix every caller the compiler finds. Rules:
   - `api/sync.rs resend_declined_as_new_transfer`: clone the stamp — read `row.layout` in the same block that clones `display_name` (~line 3448) and pass it to `engine.enqueue_package(&new_dir, display_name, files, layout)`.
   - Every OTHER desktop caller in `api/sync.rs`: pass `PackageLayout::Batch` (desktop senders are out of v1 scope).
   - Perseus `run.rs::enqueue_package_to_all`: add `layout: PackageLayout` param, forward to each `engine.enqueue_package(...)`.
   - Perseus `batcher.rs::deliver_batch` → `enqueue_package_to_all(..., PackageLayout::Batch)` placeholder (Task 6 wires the real value).
   - Perseus `resend.rs::send_batch_to_target` (~line 731 `engine.enqueue_package`): `PackageLayout::Batch` placeholder (Task 6).
   - Any engine-test mock stores implementing `SyncStore`: add the param, store or ignore.

- [ ] **Step 4: Run tests**

Run: `cargo test -p athenaeum-core --lib && cargo test -p perseus`
Expected: PASS across both crates (placeholders keep behavior identical).

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/sync/engine.rs crates/athenaeum-core/src/sync/store.rs crates/athenaeum-core/src/api/sync.rs crates/perseus/src/run.rs crates/perseus/src/batcher.rs crates/perseus/src/resend.rs
git commit -m "feat(sync): thread PackageLayout through enqueue; declined-resend clones the stamp (mirror-hierarchy T2)"
```

---

### Task 3: wire `Announce4` + transport threading + engine emission

**Files:**
- Modify: `crates/athenaeum-core/src/sharing/types.rs` (`PackageAnnounceV4` after V3 ~line 82; `TransportEvent::AnnounceReceived` gains `layout` ~line 153)
- Modify: `crates/athenaeum-core/src/sharing/iroh/proto.rs` (`Msg::Announce4` appended after `Presence` ~line 152; `announce_received_from_msg` arms ~lines 166-231)
- Modify: `crates/athenaeum-core/src/sharing/iroh/mod.rs` (announce impl ~line 1218; the accept-loop match that routes announce variants into `announce_received_from_msg` — find the `Msg::Announce | Msg::Announce2 | Msg::Announce3` guard and add `Announce4`)
- Modify: `crates/athenaeum-core/src/sharing/mod.rs` (trait `announce` ~line 131 gains `layout: PackageLayout`)
- Modify: `crates/athenaeum-core/src/sharing/loopback.rs` (announce ~line 239 threads `layout` into the event)
- Modify: `crates/athenaeum-core/src/sync/engine.rs` (announce site ~lines 1393-1471: read `row.layout` in the SAME `self.store.get_outbound(id)` match that reads `display_name`, pass to `transport.announce(...)`)
- Modify: `crates/athenaeum-core/src/sharing/wire_golden_tests.rs` (new `sample_announce_v4`, new golden triple, count 23→24; extend the legacy-fallback test with layout assertions)
- Test: golden tests + any `sharing/tests.rs` mocks the compiler flags

**Interfaces:**
- Consumes: `PackageLayout`, `OutboundRow.layout` (T1/T2).
- Produces: `SharingTransport::announce(to, a, batch_name, batch_uuid, files, layout)`; `TransportEvent::AnnounceReceived { .., layout: PackageLayout }`; `Msg::Announce4(PackageAnnounceV4)` where `PackageAnnounceV4` = V3 fields + `layout: PackageLayout`. Task 4 consumes the event field.

- [ ] **Step 1: Write the failing tests**

`wire_golden_tests.rs`:

```rust
/// V4 announce sample — the v3 fields + the receiver landing layout. Mirror on
/// purpose: the sender only ever emits Announce4 for mirror transfers.
fn sample_announce_v4() -> PackageAnnounceV4 {
    PackageAnnounceV4 {
        package_id: PackageId("pkg-uuid-1".to_string()),
        root_hash: "blake3-collection-hash".to_string(),
        byte_size: 4096,
        frame_count: 3,
        batch_name: "Туманность".to_string(),
        batch_uuid: "batch-uuid-9".to_string(),
        files: vec![sample_announce_file_entry()],
        layout: PackageLayout::Mirror,
    }
}
```

Add to `golden_cases()` (LAST, after the Presence entry), with an EMPTY expected hex for now:

```rust
        // Perseus mirror-hierarchy: appended `Announce4` (disc 0b).
        (
            "msg_announce4",
            Msg::Announce4(sample_announce_v4()).encode().unwrap(),
            "", // captured from the first run below — a NEW pin, never a re-pin
        ),
```

Bump `all_wire_types_are_pinned` from 23 to 24 with a comment line. Extend `legacy_announce_bytes_decode_with_batch_uuid_fallback`: for each of v1/v2/v3 assert the produced event has `layout == PackageLayout::Batch`; add a v4 case asserting `layout == PackageLayout::Mirror` and `batch_uuid == "batch-uuid-9"`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p athenaeum-core --lib wire_golden`
Expected: compile FAIL (`PackageAnnounceV4` unknown).

- [ ] **Step 3: Implement**

`types.rs` — after `PackageAnnounceV3`:

```rust
/// V4 package announce: the [`PackageAnnounceV3`] fields plus the receiver
/// landing [`PackageLayout`]. A SEPARATE type carried by the appended
/// `Msg::Announce4` wire variant (older announce bytes stay frozen). The app
/// sender emits v4 ONLY for `Mirror` transfers — `Batch` keeps emitting v3, so
/// peers that never enable the setting have zero compatibility exposure. An
/// old receiver cannot decode v4: the announce goes un-acked and the sender
/// retries (documented "receiver must be upgraded" stance, same as the
/// v2→v3 rollout). Byte image pinned by `sharing::wire_golden_tests`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageAnnounceV4 {
    pub package_id: PackageId,
    pub root_hash: String,
    pub byte_size: u64,
    pub frame_count: u32,
    pub batch_name: String,
    pub batch_uuid: String,
    pub files: Vec<AnnounceFileEntry>,
    pub layout: PackageLayout,
}
```

`TransportEvent::AnnounceReceived`: add `layout: PackageLayout` (doc: v1–v3 map to `Batch`; v4 carries it).

`proto.rs` — append after `Presence` (comment in the established append-block style):

```rust
    // Perseus mirror-hierarchy (spec 2026-07-27) — appended AFTER `Presence` as
    // the LAST variant; every index above stays frozen (same append-only rule).
    // The app sender emits `Announce4` ONLY for a Mirror-layout transfer;
    // Batch transfers keep emitting `Announce3`, so peers that never enable
    // the setting exchange no new bytes.
    /// Provider advertises a fetchable package with its v4 landing layout.
    Announce4(PackageAnnounceV4),
```

`announce_received_from_msg`: v1/v2/v3 arms gain `layout: PackageLayout::Batch`; new arm destructures `PackageAnnounceV4` exactly like the V3 arm and threads `layout` through. Update the fn doc (v4 carries layout).

`iroh/mod.rs` — accept-loop announce guard: add `Msg::Announce4(_)` to the match arm that feeds `announce_received_from_msg`. The `announce` impl gains `layout: PackageLayout` and branches:

```rust
        let msg = match layout {
            PackageLayout::Mirror => Msg::Announce4(PackageAnnounceV4 {
                package_id: wire.package_id,
                root_hash: wire.root_hash,
                byte_size: wire.byte_size,
                frame_count: wire.frame_count,
                batch_name: batch_name.to_string(),
                batch_uuid: batch_uuid.to_string(),
                files: files.to_vec(),
                layout,
            }),
            // Batch keeps the frozen v3 bytes — zero exposure for old peers.
            PackageLayout::Batch => Msg::Announce3(PackageAnnounceV3 {
                package_id: wire.package_id,
                root_hash: wire.root_hash,
                byte_size: wire.byte_size,
                frame_count: wire.frame_count,
                batch_name: batch_name.to_string(),
                batch_uuid: batch_uuid.to_string(),
                files: files.to_vec(),
            }),
        };
        self.send_control(to, msg).await?;
```

`sharing/mod.rs` trait `announce`: add `layout: PackageLayout` (update the doc comment: v3 for Batch, v4 for Mirror). `loopback.rs`: add the param, set `layout` on the emitted event (it mirrors exactly what the corresponding wire decode would produce — no more, no less). Fix every other implementor/caller the compiler finds (engine announce call site passes the row layout; mocks pass `PackageLayout::Batch`).

`engine.rs` announce site: extend the existing `self.store.get_outbound(id)` match to also capture `layout` (default `PackageLayout::Batch` on the `Ok(None)`/`Err` arms, mirroring the `display_name` fallbacks), pass it as the new `announce` arg. The collab `announce_project` branch is untouched.

Then capture the new pin: run the golden test once, copy the ACTUAL hex from the assertion message into the `msg_announce4` literal. Do NOT touch any other literal.

- [ ] **Step 4: Run tests**

Run: `cargo test -p athenaeum-core --lib`
Expected: PASS — all goldens (24 pinned), fallback layout assertions, engine/loopback suites.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/sharing/types.rs crates/athenaeum-core/src/sharing/iroh/proto.rs crates/athenaeum-core/src/sharing/iroh/mod.rs crates/athenaeum-core/src/sharing/mod.rs crates/athenaeum-core/src/sharing/loopback.rs crates/athenaeum-core/src/sharing/wire_golden_tests.rs crates/athenaeum-core/src/sync/engine.rs
git commit -m "feat(sharing): Announce4 carries PackageLayout; mirror-only emission (mirror-hierarchy T3)"
```

---

### Task 4: receiver — mirror layout skips the batch landing level

**Files:**
- Modify: `crates/athenaeum-core/src/sync/receiver.rs` (`process_receiver_event` destructure ~line 1210, `handle_announce` signature ~line 1857 + the `landing_override` block ~lines 2425-2448)
- Test: receiver.rs test module (one unit-style + one loopback e2e patterned on `v2_receive_lands_under_batch_and_settles_files_with_history` ~line 5259)

**Interfaces:**
- Consumes: `TransportEvent::AnnounceReceived.layout` (T3); `enqueue_package(.., layout)` (T2) to drive e2e sends.
- Produces: the user-visible behavior — mirror transfers land under `<incoming>/<sender_slug>/<rel_path>` with `landing_dir` NULL on the row.

- [ ] **Step 1: Write the failing e2e test**

Pattern on `v2_receive_lands_under_batch_and_settles_files_with_history` (same fixtures: loopback pair, temp incoming root, package dir with a nested `rel_path` manifest). New test in the same module:

```rust
    /// Mirror layout: two SEQUENTIAL mirror sends from one sender land in ONE
    /// stable tree (no batch level, adjacent files), the inbound rows persist
    /// NO landing_dir (v1-style), and a changed-content re-send of an existing
    /// rel_path lands collision-suffixed `_2` instead of overwriting.
    #[tokio::test]
    async fn mirror_layout_lands_adjacent_across_batches_and_suffixes_collisions() {
        // 1. Build package A: files ["M31/L_0001.fits", "M31/L_0002.fits"],
        //    enqueue with PackageLayout::Mirror, drive to Done (existing
        //    loopback-harness helpers).
        // 2. Build package B: ["M31/L_0003.fits"], enqueue Mirror, drive to Done.
        // 3. Assert all three files exist under ONE dir:
        //    <incoming>/<sender_slug>/M31/{L_0001,L_0002,L_0003}.fits — and that
        //    NO path component matches either batch's name/uuid.
        // 4. Assert both inbound rows have landing_dir == None.
        // 5. Build package C re-sending "M31/L_0001.fits" with DIFFERENT bytes
        //    (different content → different frame_uuid/hash so dedup lets it
        //    through), drive to Done; assert M31/L_0001_2.fits exists and the
        //    original M31/L_0001.fits is byte-unchanged.
    }
```

Write it fully by cloning the v2 test's setup verbatim and adjusting the three numbered blocks; the assertions above are the contract. Also extend one existing BATCH-path test run to confirm nothing moved (they already pin it — just run them).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p athenaeum-core --lib mirror_layout_lands`
Expected: FAIL — files land under a batch dir (layout not yet honored) or compile error on the destructure.

- [ ] **Step 3: Implement**

`process_receiver_event`: destructure `layout` from the event, pass to `handle_announce` (new param `layout: PackageLayout`; the fn already wears `#[allow(clippy::too_many_arguments)]`). The landing block becomes:

```rust
    // Mirror layout (spec 2026-07-27): NO batch landing level — land under
    // `<incoming_root>/<sender_slug>` via the pre-v2 (v1) path, which IS the
    // stable capture-mirror tree. resolve_landing_dir is deliberately not
    // called: concurrent mirror transfers from one sender must share the tree,
    // and per-file collisions are handled by ingest's unique_path.
    let landing_override: Option<PathBuf> = match layout {
        PackageLayout::Mirror => None,
        PackageLayout::Batch => match effective_name.as_deref().and_then(sanitize_batch_slug) {
            Some(batch_slug) => {
                let conn = store.lock_conn();
                Some(resolve_landing_dir(
                    &conn,
                    inbound_id,
                    &incoming_root,
                    &peer_device,
                    &batch_slug,
                ))
            }
            None => None,
        },
    };
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p athenaeum-core --lib receiver && cargo test -p athenaeum-core --lib mirror_layout`
Expected: PASS — new e2e + every existing landing/receiver test untouched.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/sync/receiver.rs
git commit -m "feat(sync): mirror layout lands in the stable sender tree (mirror-hierarchy T4)"
```

---

### Task 5: Perseus setting — TOML, send-mode PUT, web UI checkbox

**Files:**
- Modify: `crates/perseus/src/config.rs` (Config field ~line 390 block, `SendCfg` ~line 143 + `Default` + `send_cfg()` ~line 770)
- Modify: `crates/perseus/src/config_template.toml` (comment block after `auto_quiet_secs`, ~line 9)
- Modify: `crates/perseus/src/config_edit.rs` (`apply_send_mode_edit` ~line 303)
- Modify: `crates/perseus/src/web.rs` (`SendModeEdit` ~line 1543, `SendModeDto` ~line 1519, `api_get_send_mode` ~line 2380, `api_put_send_mode` ~line 2406; `PendingDto` ~line 1496 if it carries the mode fields for the 2 s poll — mirror them there too)
- Modify: `crates/perseus/src/web/app.js` (`renderTransfersTab` ~line 120-135, `applyModeControls` ~line 1063, `saveSendMode` ~line 1148, `wireTosync` ~line 1284)
- Test: config.rs + config_edit.rs test modules

**Interfaces:**
- Consumes: nothing new from core.
- Produces: `Config.mirror_hierarchy: bool` (serde default false), `SendCfg.mirror_hierarchy: bool`, `apply_send_mode_edit(.., mirror_hierarchy: Option<bool>)` (None = leave the key alone), wire DTO field `mirrorHierarchy`. Task 6 reads `SendCfg.mirror_hierarchy`.

- [ ] **Step 1: Write the failing tests**

`config.rs` tests:

```rust
    #[test]
    fn mirror_hierarchy_defaults_false_and_parses() {
        let cfg = single_root_config(); // the existing test-helper Config
        assert!(!cfg.mirror_hierarchy);
        assert!(!cfg.send_cfg().mirror_hierarchy);
        let parsed = Config::from_toml_str_lenient(&format!(
            "{}\nmirror_hierarchy = true\n",
            minimal_config_toml() // reuse whatever minimal-TOML helper the module's parse tests use
        ))
        .unwrap();
        assert!(parsed.mirror_hierarchy);
        assert!(parsed.send_cfg().mirror_hierarchy);
    }
```

`config_edit.rs` tests (clone the module's existing `apply_send_mode_edit` test shape):

```rust
    #[test]
    fn send_mode_edit_writes_and_leaves_mirror_hierarchy() {
        // Some(true) writes the key; a later None edit leaves it untouched.
        // Assert on the re-read Config: after edit 1 → true; after an edit with
        // mirror_hierarchy: None → still true.
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p perseus mirror_hierarchy`
Expected: FAIL — field unknown.

- [ ] **Step 3: Implement**

`config.rs`: in the send-key block add

```rust
    /// Receiver landing layout: `true` = every send lands in the stable
    /// capture-mirror tree on the receiver (spec 2026-07-27), `false` = the
    /// per-batch folders. Top-level key like the other send keys.
    #[serde(default)]
    pub mirror_hierarchy: bool,
```

`SendCfg`: add `pub mirror_hierarchy: bool` (+ `false` in `Default`, copy in `send_cfg()`).

`config_template.toml` after the `auto_quiet_secs` line:

```toml
# Land files on receiving devices in the same folder tree they occupy under the
# capture dir (one stable tree per device) instead of one folder per send.
# Receivers must run a version that understands this layout. Default false.
# mirror_hierarchy = true
```

`config_edit.rs::apply_send_mode_edit`: add `mirror_hierarchy: Option<bool>` param +

```rust
    if let Some(mirror) = mirror_hierarchy {
        doc["mirror_hierarchy"] = toml_edit::value(mirror);
    }
```

`web.rs`: `SendModeEdit` gains `#[serde(default)] mirror_hierarchy: Option<bool>`; `SendModeDto` gains `mirror_hierarchy: bool`; both handlers thread/echo it (`api_put_send_mode` passes `edit.mirror_hierarchy` to `apply_send_mode_edit`); if `PendingDto` carries `mode`/`autoQuietSecs` for the 2 s poll, add `mirror_hierarchy` beside them and include it in the `applyModeControls` repaint path.

`app.js`: checkbox in the To-Sync row (next to `#quietWrap`):

```html
<label class="inline-label"><input id="mirrorHier" type="checkbox" /> Mirror capture folders on receiver</label>
```

`saveSendMode`: `mirrorHierarchy: $('mirrorHier').checked` in the `edit` object; `applyModeControls` gains a `mirror` arg painted with the focus guard (`if (document.activeElement !== $('mirrorHier')) $('mirrorHier').checked = mirror === true;`) — update its three callers; `wireTosync`: `$('mirrorHier').addEventListener('change', saveSendMode);`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p perseus`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/perseus/src/config.rs crates/perseus/src/config_template.toml crates/perseus/src/config_edit.rs crates/perseus/src/web.rs crates/perseus/src/web/app.js
git commit -m "feat(perseus): mirror_hierarchy setting — TOML, send-mode PUT, To-Sync checkbox (mirror-hierarchy T5)"
```

---

### Task 6: Perseus enqueue paths use the real setting

**Files:**
- Modify: `crates/perseus/src/batcher.rs` (`SendContext` gains `send_cfg_rx: tokio::sync::watch::Receiver<SendCfg>` if it does not already hold one — clone it where the agent spawn creates the `watch::channel` pair; `deliver_batch` derives layout)
- Modify: `crates/perseus/src/run.rs` (SendContext construction site)
- Modify: `crates/perseus/src/resend.rs` (`send_batch_to_target` gains `layout: PackageLayout`, passes it to `enqueue_package`)
- Modify: `crates/perseus/src/web.rs` (`api_send_to` ~line 3558 passes the CURRENT setting; the Perseus declined-resend call site passes `row.layout`)
- Test: batcher.rs test module (reuse the T8 flush/send_explicit harness)

**Interfaces:**
- Consumes: `SendCfg.mirror_hierarchy` (T5), `enqueue_package_to_all(.., layout)` / `enqueue_package(.., layout)` (T2), `OutboundRow.layout` (T1).
- Produces: final user-facing behavior — the setting governs every fresh Perseus enqueue; a declined-resend keeps its original stamp.

- [ ] **Step 1: Write the failing test**

In batcher.rs tests, reuse the existing harness that drives `send_explicit`/`flush_once` against a real `StandaloneSyncStore`-backed engine:

```rust
    /// The live mirror_hierarchy setting stamps every fresh enqueue: flip the
    /// watch channel to mirror → the next delivered batch's sync_outbound row
    /// reads layout = mirror; flip back → batch.
    #[tokio::test]
    async fn deliver_batch_stamps_layout_from_live_send_cfg() {
        // harness setup as in the existing flush tests …
        // send_cfg_tx.send(SendCfg { mirror_hierarchy: true, ..base.clone() }).unwrap();
        // deliver one batch; read the engine store's newest outbound row:
        // assert_eq!(row.layout, PackageLayout::Mirror);
        // send_cfg_tx.send(base).unwrap(); deliver another;
        // assert_eq!(row2.layout, PackageLayout::Batch);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p perseus deliver_batch_stamps_layout`
Expected: FAIL — rows always `batch` (placeholder from T2).

- [ ] **Step 3: Implement**

`deliver_batch` replaces its T2 placeholder:

```rust
    let layout = if ctx.send_cfg_rx.borrow().mirror_hierarchy {
        PackageLayout::Mirror
    } else {
        PackageLayout::Batch
    };
```

and passes `layout` to `enqueue_package_to_all`. This single point covers auto, scheduled, `/api/send-now`, AND Library `send_explicit` (all funnel through `deliver_batch`). If `SendContext` lacks the receiver, add it and clone from the same `watch::channel(SendCfg)` pair the supervisor/web already share (the `send_cfg_tx` trait seam, supervisor.rs:115) at the SendContext construction site in run.rs.

`send_batch_to_target`: new `layout: PackageLayout` param → `enqueue_package(&new_dir, display_name, files, layout)`. Callers: `api_send_to` derives from the live config (`if state.config.read().await.mirror_hierarchy { Mirror } else { Batch }`); the Perseus declined-resend path passes `row.layout` (the cloned stamp, per spec §1).

- [ ] **Step 4: Run tests**

Run: `cargo test -p perseus && cargo test -p athenaeum-core --lib`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/perseus/src/batcher.rs crates/perseus/src/run.rs crates/perseus/src/resend.rs crates/perseus/src/web.rs
git commit -m "feat(perseus): live mirror_hierarchy setting stamps every send path (mirror-hierarchy T6)"
```

---

### Task 7: docs, ledger, full gates

**Files:**
- Modify: `CLAUDE.md` (Transfers section: one bullet on mirror layout — setting, Announce4-only-for-mirror, v1-path landing, receiver version requirement)
- Modify: `.superpowers/sdd/progress.md` (new section "PERSEUS MIRROR HIERARCHY — SDD ledger" with per-task commit hashes and any deferred minors)
- NOT in this task: the end-user artfrom-space docs (already tracked as the recorded owner doc TODO) and session-memory updates.

**Steps:**

- [ ] **Step 1: Write the CLAUDE.md bullet** (Transfers/Perseus block, one dense bullet in the established style: setting name, stamp-at-enqueue + declined-resend clone, Announce4 mirror-only emission + old-receiver stance, v1-landing realization, unique_path collisions, additive-sync boundaries).
- [ ] **Step 2: Write the ledger section** with the actual commit hashes of T1–T6 and any review follow-ups.
- [ ] **Step 3: Full gates**

Run: `cargo test -p athenaeum-core --lib && cargo test -p perseus && cargo build -p athenaeum-web`
Expected: all PASS (workspace default-features caveat from Global Constraints applies to `cargo build --workspace` only — do not chase it).

- [ ] **Step 4: Commit**

```bash
git add CLAUDE.md .superpowers/sdd/progress.md
git commit -m "docs: mirror-hierarchy CLAUDE.md bullet + SDD ledger (mirror-hierarchy T7)"
```

Owner smokes owed after the cycle (record in ledger, do not attempt in-session): Perseus → desktop two manual sends from one directory with the setting ON (adjacency), toggle OFF (per-batch returns), old-receiver announce stance sanity, Windows receiver path check.

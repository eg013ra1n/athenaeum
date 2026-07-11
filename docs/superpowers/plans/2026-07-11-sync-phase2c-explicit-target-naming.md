# Sync Phase 2C — Explicit-Target Send + Capability Model + Node Naming — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the device-role/pairing send model with a device-**capability** model + **explicit-target** sends, add **cross-OS node naming** (default hostname, hub-`PATCH` editable), and rewrite **Perseus** to register as `perseus`, send to configured `targets`, and mirror its capture-dir tree.

**Architecture:** Build on the approved spec `docs/superpowers/specs/2026-07-11-sync-model-phase1-design.md` (§3 capability, §5 names, §6 mirror — landed in 2B, §8 targets, §13 client decisions). Plan 2A landed the account-wide allow-list + signed-in autostart; Plan 2B landed the mirror landing. This slice removes the still-live role/pairing machinery (`set_role`, `account_pairing`, `resolve_capture_peer`, `auto_enqueue_scanned_files`, `ACCOUNT_ROLE`/`ACCOUNT_PEER_DEVICE_ID`) and the whole app auto-send path, replaces the single-peer `SyncSenderRuntime` with a per-peer engine map addressed by an explicit destination, and rewrites Perseus's single-primary pairing into a multi-target multi-engine sender.

**Tech Stack:** Rust (athenaeum-core / athenaeum-tauri / athenaeum-web / perseus), iroh + iroh-blobs transport (unchanged), React/TS frontend. New dep: `hostname` crate (cross-OS hostname) in athenaeum-core.

**Ships together with the hub deploy.** The hub (Plan 1 — `capability` on `/auth/verify`, `capability` in `GET /devices`, `PATCH /devices/{id}`, `UNIQUE(account_id, lower(name))`) is merged to `athenaeum-hub` main but **not deployed**. This client work assumes that hub contract; both deploy together so the removed `role` contract never breaks a live client. Do **not** deploy from this plan.

## Global Constraints

- **Two backends in sync.** Every Tauri command touched/added here (`crates/athenaeum-tauri/src/commands/*`) has a mirror Axum route (`crates/athenaeum-web/src/routes/*`) in the same task. Real logic stays in `athenaeum-core`; the host layer is a thin wrapper.
- **Serde boundary snake_case ↔ camelCase.** `#[serde(rename_all = "camelCase")]` on wire structs; verify `src/types/models.ts` after regeneration.
- **`models.ts` is generated** from `athenaeum-core/src/ts_export.rs` — never hand-edit. Regenerate with `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`, then commit the regenerated file.
- **Never swallow errors** silently — log to `tracing` before returning at the command boundary (`#[tracing::instrument(skip_all, err)]` stays on every command/route).
- **Capability values are the exact strings** `athenaeum` and `perseus` (lowercase, serde `rename_all = "lowercase"`). The hub's `deviceCapability` verify field and `capability` device field use these verbatim.
- **Path/name safety unchanged.** Node names become directory slugs on the receiver via the 2B `sync::ingest::sanitize_slug` (receiver-resolved, never from the package). This plan does not touch that; it only feeds better names in.
- **Author every commit as `eg013ra1n <vilen.sharifov@gmail.com>`; no Claude co-author/footer.** GitLab `origin` only.
- **Gates per task:** `cargo build -p <crate>` + the crate's `cargo test` green and warning-free; frontend tasks also `npx tsc --noEmit`. The hub suite is not run here (separate repo, Plan 1).
- **Branch `0.4.0`.** No feature branch; commit directly as the prior 2A/2B tasks did.

---

### Task 1: Capability model — `DeviceCapability` replaces `DeviceRole` (core + hub client + TS)

Introduce the capability type and thread it through the account layer and hub client. This is the type foundation every later task builds on. **Behavioral change:** `verify` now sends `deviceCapability`; `GET /devices` is decoded with `capability`. **No** send/role logic is removed yet (Task 2) — this task keeps the suite green by leaving `set_role`/`account_pairing` compiling against the new type where they still read a capability-shaped value, or by leaving them referencing a temporary until Task 2 deletes them. Prefer: keep `DeviceRole`-consuming functions compiling by having them read the new `capability` where trivial, and **defer their deletion to Task 2** (do not delete send/pairing functions here).

**Files:**
- Modify: `crates/athenaeum-core/src/account/mod.rs` (add `DeviceCapability`; `AccountDevice.capability`, drop `role`/`peer_device_id`; `AccountStatus.capability`, drop `role`)
- Modify: `crates/athenaeum-core/src/account/client.rs` (`verify` sends `deviceCapability`; `list_devices` decodes `capability`; delete `set_role` method **in Task 2**, keep here)
- Modify: `crates/athenaeum-core/src/api/account.rs` (`build_status` fills `capability = Athenaeum`; `sign_in_verify` passes `DeviceCapability::Athenaeum` to `verify`; drop `refresh_persisted_role`'s role write **in Task 2** — here just make it compile)
- Modify: `crates/athenaeum-core/src/ts_export.rs` (register `DeviceCapability`, drop `DeviceRole` registration)
- Modify: `crates/athenaeum-core/src/sync/status.rs` (`SyncStatus.machine_role` — retype to `Option<DeviceCapability>` **or** leave for Task 2; simplest: keep the field name, retype to `DeviceCapability` is wrong — Task 2 removes it. Here: change the type import so the crate compiles.)
- Regenerate: `src/types/models.ts`
- Test: `crates/athenaeum-core/src/account/mod.rs` inline `#[cfg(test)]`; `crates/athenaeum-core/src/account/client.rs` inline tests

**Interfaces:**
- Produces:
  ```rust
  // account/mod.rs
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
  #[serde(rename_all = "lowercase")]
  pub enum DeviceCapability { Athenaeum, Perseus }
  impl DeviceCapability {
      pub fn as_str(self) -> &'static str {
          match self { DeviceCapability::Athenaeum => "athenaeum", DeviceCapability::Perseus => "perseus" }
      }
      pub fn parse(s: &str) -> Option<Self> {
          match s { "athenaeum" => Some(Self::Athenaeum), "perseus" => Some(Self::Perseus), _ => None }
      }
  }
  impl Default for DeviceCapability { fn default() -> Self { DeviceCapability::Athenaeum } }
  ```
  ```rust
  pub struct AccountDevice { pub id: String, pub name: String, pub pubkey: String,
      #[serde(default)] pub capability: DeviceCapability, pub created_at: String, pub last_seen_at: Option<String> }
  pub struct AccountStatus { pub signed_in: bool, pub email: Option<String>,
      pub device_id: Option<String>, pub capability: DeviceCapability, pub hub_url: String }
  // client.rs
  pub async fn verify(&self, email: &str, code: &str, device_pubkey_b64: &str,
      device_name: &str, capability: DeviceCapability) -> Result<VerifyResponse, AccountClientError>;
  ```
- Consumes: hub JSON `GET /devices` now carries `"capability": "athenaeum"|"perseus"`; `POST /auth/verify` accepts `"deviceCapability"`.

- [ ] **Step 1: Write failing test** — capability round-trip + AccountDevice decode

```rust
// account/mod.rs tests
#[test]
fn device_capability_str_roundtrip() {
    assert_eq!(DeviceCapability::Athenaeum.as_str(), "athenaeum");
    assert_eq!(DeviceCapability::Perseus.as_str(), "perseus");
    assert_eq!(DeviceCapability::parse("perseus"), Some(DeviceCapability::Perseus));
    assert_eq!(DeviceCapability::parse("bogus"), None);
    assert_eq!(DeviceCapability::default(), DeviceCapability::Athenaeum);
}
#[test]
fn account_device_decodes_capability_and_defaults_athenaeum() {
    let with: AccountDevice = serde_json::from_value(serde_json::json!({
        "id":"d1","name":"Studio Mac","pubkey":"AAAA","capability":"perseus","createdAt":"t"
    })).unwrap();
    assert_eq!(with.capability, DeviceCapability::Perseus);
    let without: AccountDevice = serde_json::from_value(serde_json::json!({
        "id":"d2","name":"Laptop","pubkey":"BBBB","createdAt":"t"
    })).unwrap();
    assert_eq!(without.capability, DeviceCapability::Athenaeum); // missing → default
}
```

- [ ] **Step 2: Run, confirm fail** — `cargo test -p athenaeum-core --lib device_capability_str_roundtrip account_device_decodes_capability_and_defaults_athenaeum` → FAIL (type absent).

- [ ] **Step 3: Implement** — add `DeviceCapability` (code above); change `AccountDevice` (drop `role`, `peer_device_id`; add `capability` with `#[serde(default)]`); change `AccountStatus` (drop `role`; add `capability`). In `client.rs::verify` add `capability: DeviceCapability` param and put `"deviceCapability": capability.as_str()` in the JSON body; `list_devices` decodes automatically via the new struct. In `api/account.rs::build_status` set `capability: DeviceCapability::Athenaeum` (the app is always a full peer) and drop the `role` read; in `sign_in_verify` pass `DeviceCapability::Athenaeum` to `verify`. Retarget `ts_export.rs` (replace the `DeviceRole` line with `DeviceCapability`). Make `sync/status.rs` and any `DeviceRole` references compile — where a function is scheduled for deletion in Task 2 (`refresh_persisted_role`, `derive_pairing_summary`, `set_machine_role`, `account_pairing`, `machine_role`, `peer_device_id`, `auto_mode_ready`, `retention_ready` role read), leave them compiling by the minimal change (e.g. stop reading `me.role`; read nothing / a placeholder), because Task 2 deletes them wholesale.

  > Implementer note: keeping half-dead functions compiling for one task is deliberate — it keeps Task 1 a pure type-foundation with a green suite, and Task 2 is a clean deletion pass. Do **not** try to also delete the send/pairing path here.

- [ ] **Step 4: Run** — the two tests + `cargo test -p athenaeum-core` green, warning-free (allow `dead_code` only if unavoidable and deleted next task; prefer `#[allow(dead_code)]` with a `// Task 2 removes this` note over leaving a warning).

- [ ] **Step 5: Regenerate TS** — `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`; confirm `src/types/models.ts` now has `export type DeviceCapability = "athenaeum" | "perseus";`, `AccountDevice.capability`, `AccountStatus.capability`, and no `DeviceRole`. `npx tsc --noEmit` will fail on frontend `role` reads — that is expected and fixed in Task 5; **do not** touch frontend here. Note the tsc breakage in the commit body.

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-core/src/account/ crates/athenaeum-core/src/api/account.rs crates/athenaeum-core/src/ts_export.rs crates/athenaeum-core/src/sync/status.rs src/types/models.ts
git commit -m "feat(account): DeviceCapability model — capability on verify + /devices, replaces DeviceRole"
```

---

### Task 2: Remove role/pairing/auto-send machinery (core + tauri + web)

Delete the app's **auto-send** and the role-**write** machinery (role selector, `set_role`, role readers). After this task the app **receives** (Plan 2A autostart) but never auto-sends. This is a compiler-guided deletion pass — remove the symbols, fix callers until green.

**Task-boundary rule (why the send primitive stays):** the manual send chain `enqueue_sync_selection → ensure_sender_engine → resolve_capture_peer → account_pairing` compiles today only because of the role/pairing readers. Task 3 replaces that whole chain with the explicit-target primitive. So **Task 2 leaves the send-primitive chain compiling and untouched** (`account_pairing`, `resolve_capture_peer`, `persist_peer_resolution`, `peer_from_resolution`, `ensure_sender_engine`, `enqueue_sync_selection`, `sync/pairing.rs` resolution, the `DeviceRole` enum, and the `ACCOUNT_ROLE`/`ACCOUNT_PEER_DEVICE_ID` **consts**). With `set_machine_role` gone, `ACCOUNT_ROLE` is never written → `account_pairing` returns `None` → the manual send is transitionally inert until Task 3 rebuilds it. **Task 3 deletes** `resolve_capture_peer`/`account_pairing`/`peer_from_resolution`/`DeviceRole`/the two consts/`sync/pairing.rs` primary-resolution when it installs the explicit-target replacement.

**Remove in THIS task (symbol @ file:line — verify current line by symbol name):**
- `api/account.rs`: `set_machine_role` (~L394), `refresh_persisted_role` (already a no-op stub after Task 1 — delete it + its call site in `sign_in_verify` ~L262). Leave the `ACCOUNT_ROLE`/`ACCOUNT_PEER_DEVICE_ID` clears in `clear_local_session`/`sign_in_verify` in place (they reference the consts Task 3 removes; harmless).
- `account/client.rs`: `set_role` method (~L209) + its tests (~L366, L407, L489).
- `api/sync.rs`: `machine_role` (~L488), `peer_device_id` (~L496), `auto_mode_ready` (~L1064), `auto_enqueue_scanned_files` (~L1088), `derive_pairing_summary` (~L513), `pairing_summary_from` (~L529) + their tests (~L1613-1651, L1502-1520). **Keep** `resolve_capture_peer`, `persist_peer_resolution`, `peer_from_resolution`, `ensure_sender_engine`, `enqueue_sync_selection` (Task 3 owns them).
- `api/retention.rs`: the `ACCOUNT_ROLE`/`DeviceRole::Capture` gate in `retention_ready` (~L205-207) → replace with signed-in gate: `Ok(hub_credentials(ctx)?.is_some())`.
- `sync/status.rs`: `SyncStatus.machine_role` field (already `#[ts(skip)]` after Task 1) — remove the field; update `get_status` (`api/sync.rs` ~L612/L630) to stop populating it. Also remove `SyncStatus.pairing_summary` (fed by `derive_pairing_summary`) and its `get_status` population.
- `api/account.rs` tauri/web: `set_machine_role` command (`athenaeum-tauri/src/commands/account.rs` ~L74; registered `lib.rs` ~L469) and route (`athenaeum-web/src/routes/account.rs` ~L113; registered `routes/mod.rs` ~L263), plus their request DTOs (`role: DeviceRole`).
- `auto_enqueue_scanned_files` call sites: `athenaeum-tauri/src/commands/scan_roots.rs:213`, `athenaeum-tauri/src/commands/sync.rs:117`, `athenaeum-web/src/routes/scan_roots.rs:253`, `athenaeum-web/src/routes/sync.rs:151`. Remove the auto-enqueue block (the scan-completion hook no longer sends). Keep the scan itself.
- `monitor/orchestrator.rs`: test `TestHook` role writes (~L361-362) + the `auto_enqueue_scanned_files` test call (~L399-415) — delete or retarget those tests.
- `sync/pairing.rs`: clean up the stale `"role": null, "peerDeviceId": null` keys in the `devices_body_without_pinned_peer` fixture (Task 1 Minor).

**Files:** the above; Test: adjust/remove the now-invalid inline tests; keep `enqueue_sync_selection`/`ensure_sender_engine`'s own tests (Task 3 re-points them). After removing `SyncStatus` fields, regenerate TS (`TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`) and commit `models.ts`.

- [ ] **Step 1: Write failing test** — assert the auto-send hook is gone at the type level (a compile check) and retention gate flipped:

```rust
// api/retention.rs tests
#[test]
fn retention_ready_is_signed_in_not_role() {
    let ctx = super::super::test_ctx(); // reuse existing retention test ctx builder
    // signed out → not ready
    assert!(!retention_ready(&ctx).unwrap());
    set_hub_credentials(&ctx); // existing helper that writes ACCOUNT_HUB_URL + token
    assert!(retention_ready(&ctx).unwrap(), "any signed-in node runs sender-copy retention");
}
```
(Mirror the existing retention test harness; if none exists inline, add the minimal ctx builder used by the retention module's other tests.)

- [ ] **Step 2: Run, confirm fail** — `cargo test -p athenaeum-core --lib retention_ready_is_signed_in_not_role` → FAIL (old gate reads role).

- [ ] **Step 3: Implement** — perform the deletions above; flip `retention_ready`; delete the two tauri commands + two web routes and their `invoke_handler!`/`build_router` registrations; delete the four auto-enqueue call blocks. Follow the compiler until `cargo build --workspace` is clean.

- [ ] **Step 4: Run** — the new retention test + `cargo test -p athenaeum-core` + `cargo build --workspace` green, warning-free. `npx tsc --noEmit` still red on frontend `role`/`setRole`/`machineRole` (Task 5).

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core crates/athenaeum-tauri crates/athenaeum-web
git commit -m "refactor(sync): remove role/pairing send-gating + app auto-send (mesh model)"
```

---

### Task 3: Explicit-target send — per-peer sender runtime (core + tauri + web)

Replace the single-peer `SyncSenderRuntime` with a per-peer engine map, and make `ensure_sender_engine` / `enqueue_sync_selection` take an explicit destination node. The destination is a device **id** resolved to a `NodeId` via the cached account device list (same resolver the receiver allow-list uses). This is the send primitive Perseus (Task 7) and the Phase-3 app UI consume.

**Deferred deletions this task also performs (kept compiling through Task 2):** delete `resolve_capture_peer`, `persist_peer_resolution`, `peer_from_resolution`, `account_pairing` (`api/account.rs`), the `DeviceRole` enum + `AccountPairing`/`PeerResolution` and `sync/pairing.rs::fetch_primary_node_id`/`resolve_peer` primary-resolution (keep `peer_addr_with_relays` + relay helpers — the new `ensure_sender_engine` dials with them), and the `ACCOUNT_ROLE`/`ACCOUNT_PEER_DEVICE_ID` consts (`settings/mod.rs`) plus their now-orphaned clears in `clear_local_session`/`sign_in_verify`. After this task, zero `DeviceRole`/role/pairing symbols remain in core. Regenerate TS if any exported type changed.

**Files:**
- Modify: `crates/athenaeum-core/src/sync/sender.rs` (map keyed by `NodeId`)
- Modify: `crates/athenaeum-core/src/api/sync.rs` (`ensure_sender_engine(ctx, sender, dest, emitter)`, `enqueue_sync_selection(ctx, sender, dest, frame_ids, emitter)`, a `resolve_dest_node(ctx, device_id) -> NodeId` helper)
- Modify: `crates/athenaeum-tauri/src/commands/sync.rs` + `crates/athenaeum-web/src/routes/sync.rs` (`enqueue_sync_selection` command/route gains a `destinationDeviceId: String` arg)
- Modify: `crates/athenaeum-core/src/monitor/orchestrator.rs` test seams (per-peer `StartedSender` insertion)
- Test: `crates/athenaeum-core/src/api/sync.rs` inline; `crates/athenaeum-core/tests/sync_e2e.rs` (two-instance harness — pass explicit dest)

**Interfaces:**
- Produces:
  ```rust
  // sync/sender.rs — inner becomes a map
  pub struct SyncSenderRuntime { inner: tokio::sync::Mutex<std::collections::HashMap<NodeId, StartedSender>> }
  impl SyncSenderRuntime {
      pub async fn current_for(&self, peer: &NodeId) -> Option<(Arc<SyncEngineHandle>, String)>;
      pub async fn lock_inner(&self) -> tokio::sync::MutexGuard<'_, HashMap<NodeId, StartedSender>>;
      pub async fn started_peers(&self) -> Vec<NodeId>;
  }
  // api/sync.rs
  pub async fn ensure_sender_engine(ctx: &ServiceContext, sender: &SyncSenderRuntime,
      dest: NodeId, emitter: Option<Arc<dyn ProgressEmitter>>)
      -> Result<(Arc<SyncEngineHandle>, String), ApiError>;
  pub async fn enqueue_sync_selection(ctx: &ServiceContext, sender: &SyncSenderRuntime,
      dest: NodeId, frame_ids: Vec<i64>, emitter: Option<Arc<dyn ProgressEmitter>>)
      -> Result<EnqueueSelectionResult, ApiError>;
  /// Resolve an account device id → its NodeId via the cached device list.
  pub async fn resolve_dest_node(ctx: &ServiceContext, device_id: &str) -> Result<NodeId, ApiError>;
  ```
- Consumes: `account::HubClient::list_devices` (cached), `pairing::peer_addr_with_relays` (dial hint, kept from Task 2).

- [ ] **Step 1: Write failing test** — per-peer engine isolation + resolver

```rust
// api/sync.rs tests
#[tokio::test]
async fn sender_runtime_holds_one_engine_per_peer() {
    let sender = SyncSenderRuntime::new();
    let a: NodeId = node_id_from_hex(&"aa".repeat(32));
    let b: NodeId = node_id_from_hex(&"bb".repeat(32));
    { let mut g = sender.lock_inner().await;
      g.insert(a, fake_started(a)); g.insert(b, fake_started(b)); }
    assert!(sender.current_for(&a).await.is_some());
    assert!(sender.current_for(&b).await.is_some());
    let c: NodeId = node_id_from_hex(&"cc".repeat(32));
    assert!(sender.current_for(&c).await.is_none(), "unknown peer has no engine");
    assert_eq!(sender.started_peers().await.len(), 2);
}
```
(`fake_started`/`node_id_from_hex`: reuse the existing test helpers that build a loopback `StartedSender` — the monitor/orchestrator tests already construct `StartedSender { engine, origin_device, peer }`.)

- [ ] **Step 2: Run, confirm fail** — `cargo test -p athenaeum-core --lib sender_runtime_holds_one_engine_per_peer` → FAIL (`current_for`/map absent).

- [ ] **Step 3: Implement** —
  - `sender.rs`: `inner` → `HashMap<NodeId, StartedSender>`; `current_for(peer)` looks up by key; `lock_inner` returns the map guard; add `started_peers()`. Drop `is_started()`/`current()` (single-peer) or reimplement `is_started` as `!map.is_empty()`.
  - `api/sync.rs`: fold `peer_from_resolution` away; `ensure_sender_engine` now takes `dest: NodeId`, locks the map, short-circuits if `dest` already present, else builds the transport (unchanged: relays, DeviceKey, `IrohTransport`, `blobs_out` FsStore, `peer_addr_with_relays(dest, relays)` dial hint, `CatalogSyncStore`, `SyncEngine::spawn_with_emitter`) and inserts `StartedSender { engine, origin_device, peer: dest }` under `dest`. `enqueue_sync_selection` takes `dest`, calls `ensure_sender_engine(ctx, sender, dest, emitter)`, then `build_and_enqueue_selection` against that engine. Add `resolve_dest_node`: `list_devices` (cached/hub) → find `id == device_id` → decode `pubkey` base64 → `NodeId`; error `ApiError::Invalid` if absent/undecodable or if the device's `capability == Perseus` (Perseus is never a valid destination — spec §10).
  - tauri/web `enqueue_sync_selection`: add `destination_device_id: String` (camelCase `destinationDeviceId`), call `resolve_dest_node` then the core fn.
  - Fix `sync_e2e.rs` + orchestrator test seams to insert/address per-peer.

- [ ] **Step 4: Run** — new test + `cargo test -p athenaeum-core` + `cargo test --test sync_e2e` + `cargo build --workspace` green, warning-free.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core crates/athenaeum-tauri crates/athenaeum-web
git commit -m "feat(sync): explicit-target send — per-peer sender runtime addressed by destination device id"
```

---

### Task 4: Cross-OS hostname default + `PATCH` device rename (core + tauri + web)

Add a real cross-platform hostname helper (default node name) and the `rename_device` path (client `PATCH` → hub, plus command/route). Node-name **editing** UI is Task 5 (app) / Task 7 (Perseus); this task provides the plumbing.

**Files:**
- Modify: `crates/athenaeum-core/Cargo.toml` (add `hostname = "0.4"`)
- Create: `crates/athenaeum-core/src/account/naming.rs` (`default_device_name`) — or add to `account/mod.rs`
- Modify: `crates/athenaeum-core/src/account/client.rs` (`rename_device` method → `PATCH /api/v1/devices/{id}`)
- Modify: `crates/athenaeum-core/src/api/account.rs` (`rename_device` orchestration; use `default_device_name` in `sign_in_verify` instead of the env-only `device_name()`)
- Modify: `crates/athenaeum-tauri/src/commands/account.rs` + `crates/athenaeum-web/src/routes/account.rs` (`rename_device` command/route)
- Modify: `crates/athenaeum-tauri/src/lib.rs` (`invoke_handler!`), `crates/athenaeum-web/src/routes/mod.rs` (`build_router`)
- Test: `crates/athenaeum-core/src/account/naming.rs` inline; `crates/athenaeum-core/src/account/client.rs` (wiremock-style or the crate's existing hub-client test pattern)

**Interfaces:**
- Produces:
  ```rust
  // account/naming.rs
  /// Default node name: the machine hostname, else "<prefix>-<short6>" from the node id.
  pub fn default_device_name(prefix: &str, node_id_hex: &str) -> String {
      let host = hostname::get().ok().and_then(|s| s.into_string().ok())
          .map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
      match host { Some(h) => h, None => format!("{prefix}-{}", &node_id_hex.chars().take(6).collect::<String>()) }
  }
  // client.rs
  pub async fn rename_device(&self, token: &str, device_id: &str, name: &str)
      -> Result<(), AccountClientError>; // PATCH /devices/{id} { "name": name }; 409 -> DuplicateName
  // api/account.rs
  pub async fn rename_device(ctx: &ServiceContext, device_id: String, name: String)
      -> Result<AccountStatus, ApiError>;
  ```
- Consumes: hub `PATCH /api/v1/devices/{id}` (Plan 1). 409 (duplicate name) → a typed `AccountClientError::DuplicateName` surfaced as `ApiError::Invalid("name already in use")` so the UI can suggest a suffix.

- [ ] **Step 1: Write failing test**

```rust
// account/naming.rs tests
#[test]
fn default_name_falls_back_to_prefix_short_id_when_no_host() {
    // can't unset the real hostname; test the fallback formatter directly:
    let n = fallback_name("perseus", "ab12cd34ef56...");
    assert_eq!(n, "perseus-ab12cd");
}
```
(Factor the fallback into `fn fallback_name(prefix, node_id_hex) -> String` so it is unit-testable without mutating the host environment; `default_device_name` calls it.)

- [ ] **Step 2: Run, confirm fail** — `cargo test -p athenaeum-core --lib default_name_falls_back_to_prefix_short_id_when_no_host` → FAIL.

- [ ] **Step 3: Implement** — add `hostname` dep; `naming.rs` with `default_device_name` + `fallback_name`; `client.rs::rename_device` (`PATCH`, JSON `{name}`, map 409→`DuplicateName`, 401→`Unauthorized`); `api::account::rename_device` (read token+device_id from settings if `device_id` is self, call client, return refreshed `build_status`); replace `sign_in_verify`'s `device_name()` call with `default_device_name("athenaeum", &node_id_hex)`; add the tauri command + web route (`{ deviceId, name }`), register both.

- [ ] **Step 4: Run** — new test + client rename test + `cargo test -p athenaeum-core` + `cargo build --workspace` green, warning-free.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core crates/athenaeum-tauri crates/athenaeum-web
git commit -m "feat(account): cross-OS hostname default name + PATCH device rename (command/route/client)"
```

---

### Task 5: Frontend — drop role selector, add capability display + inline name editor (app)

Rewrite the account UI for the capability model: no role radios/peer picker, a static capability line, an inline editable device name (→ `rename_device`), and capability in the device list. Fix the three other role consumers.

**Files:**
- Modify: `src/hooks/useAccount.ts` (drop `setRole`, add `renameDevice`)
- Modify: `src/components/settings/AccountSection.tsx` (remove `RoleBadge`/role state/`applyRole`/`handleSelectRole`/the "Machine role" block ~L555-635; add capability line + name editor; device-list "Role"→"Capability")
- Modify: `src/components/settings/SyncSection.tsx` (drop `RoleBadge`/role display ~L225-240; the `isCapture` auto-send gating — remove the auto-send UI: app auto-send is gone, so the Automatic-send section is removed/hidden)
- Modify: `src/hooks/useSyncSend.ts` (`canSend` no longer role-gated — set `false` for Phase 1; the app has no send UI until Phase 3)
- Modify: `src/hooks/useSyncStatus.ts` (visibility ~L176: replace `status.machineRole != null` with `(status.sender != null || status.receiver != null || status.devPairingEnabled)`)
- Consumes: regenerated `src/types/models.ts` from Task 1 (`DeviceCapability`, `capability` fields; `SyncStatus.machineRole` removed in Task 2)

**Interfaces:**
- `useAccount`: add `renameDevice: (deviceId: string, name: string) => Promise<Healed | void>` calling `api.invoke('rename_device', { deviceId, name })` then `refreshDevices()`+`refreshStatus()` (mirror `revokeDevice`'s self-healing catch → `SIGNED_OUT_HEALED`).

- [ ] **Step 1: Write failing test** — component/hook test if the suite has one (`src/**/__tests__` or vitest). If the frontend has **no** test runner wired (check `package.json`), skip a unit test and make the gate `npx tsc --noEmit` + a manual render note; state this honestly in the report. Do not fabricate a test harness.

- [ ] **Step 2: Confirm current `tsc` failure** — `npx tsc --noEmit` currently errors on `status.role`, `setRole`, `d.role`, `machineRole`, `DeviceRole` (from Task 1's regenerated types). Capture the error list as the RED.

- [ ] **Step 3: Implement** —
  - `useAccount.ts`: remove `setRole` (type L58-62, impl L178-196, return L232); add `renameDevice`; drop the `DeviceRole` import.
  - `AccountSection.tsx`: remove `RoleBadge` def, `roleDraft`/`peerDraft`/`roleError`/`roleSaving`, `currentRole`/`currentPeer`/`peerCandidates`, the two role/peer effects, `applyRole`, `handleSelectRole`, the "Machine role" JSX block, the two `RoleBadge` usages, and the "Role" `<th>`. Add: a static "Capability: full peer (athenaeum)" line in the account card, and an inline name editor (mirror `HubUrlDevEditor`'s load/edit/save shape) seeded from `thisDevice?.name`, saving via `renameDevice(deviceId, name)`, showing a duplicate-name error inline. Device list: header "Capability", cell shows `d.capability`.
  - `SyncSection.tsx`: remove `RoleBadge` + the "Machine role" display; remove the `isCapture`-gated Automatic-send section (app auto-send no longer exists) — or replace with a short "Sync is managed per device; sends are explicit" note. Remove the `DeviceRole` import and `role`/`isCapture` reads.
  - `useSyncSend.ts`: `setCanSend(false)` (Phase-1 app has no send UI; keep the hook shape for Phase 3).
  - `useSyncStatus.ts`: swap the `machineRole` visibility predicate.

- [ ] **Step 4: Run** — `npx tsc --noEmit` clean; if a test runner exists, its account/sync tests green.

- [ ] **Step 5: Commit**

```bash
git add src/
git commit -m "feat(ui): capability + editable node name in Account; drop role selector and app auto-send UI"
```

---

### Task 6: Perseus — capability registration + hostname name + `targets` config + capture-dir-relative `rel_path`

Rewire Perseus's registration off role/pairing onto the `perseus` capability, default its name to the hostname, add a `targets` config list, and build `rel_path` relative to the owning `capture_dir` (with a per-dir label when watching more than one). Multi-engine send is Task 7 — here `rel_path` + config + registration only, keeping the single-engine path compiling against the first target.

**Files:**
- Modify: `crates/perseus/src/account.rs` (`verify_and_register`: pass `DeviceCapability::Perseus` to `verify`, drop `set_role`/`auto_pick_primary`/primary-pairing; `device_name()` → core `default_device_name` + config override)
- Modify: `crates/perseus/src/config.rs` + `src/config_template.toml` (`targets: Vec<String>`; keep `[account]` but drop the `primary_device_id` requirement from `validate_ready`; add optional `device_name: Option<String>`)
- Modify: `crates/perseus/src/run.rs` (`build_package_for_file` takes the owning `capture_dir`; `rel_path` = path relative to it, forward-slash, `validate_rel_path`-clean; when `capture_dirs_resolved().len() > 1`, prefix the sanitized `capture_dir` basename as a label segment)
- Modify: `crates/perseus/src/watcher.rs` (carry the owning `capture_dir` alongside each stable path so `run.rs` can compute the relative path)
- Test: `crates/perseus/src/run.rs` inline (`rel_path` relative + multi-dir label); `crates/perseus/src/config.rs` inline (`targets` parse)

**Interfaces:**
- Produces:
  ```rust
  // run.rs
  pub fn build_package_for_file(config: &Config, capture_dir: &Path, file_path: &Path,
      origin_device: &str) -> Result<PathBuf>; // rel_path relative to capture_dir (+ label if multi-root)
  // config.rs
  pub struct Config { /* … */ pub targets: Vec<String>, pub device_name: Option<String> }
  ```
- Consumes: `athenaeum_core::account::{DeviceCapability, default_device_name}` (Tasks 1, 4); `HubClient::verify(.., DeviceCapability::Perseus)`.

- [ ] **Step 1: Write failing test**

```rust
// run.rs tests
#[test]
fn rel_path_is_relative_to_capture_dir() {
    let cfg = single_root_config("/data/astro"); // one capture_dir → no label
    let rel = compute_rel_path(&cfg, Path::new("/data/astro"), Path::new("/data/astro/M31/2026-07-10/L_0001.fits"));
    assert_eq!(rel, "M31/2026-07-10/L_0001.fits");
}
#[test]
fn rel_path_gets_root_label_when_multi_root() {
    let cfg = multi_root_config(&["/data/astro", "/mnt/backup"]);
    let rel = compute_rel_path(&cfg, Path::new("/mnt/backup"), Path::new("/mnt/backup/M31/L_0001.fits"));
    assert_eq!(rel, "backup/M31/L_0001.fits"); // sanitized basename label prefix
}
```
(Factor the path math into `fn compute_rel_path(config, capture_dir, file_path) -> String` so it is testable without writing a package.)

- [ ] **Step 2: Run, confirm fail** — `cargo test -p perseus --lib rel_path_is_relative_to_capture_dir rel_path_gets_root_label_when_multi_root` → FAIL.

- [ ] **Step 3: Implement** — `compute_rel_path` (strip_prefix, `to_slash`, label when `capture_dirs_resolved().len() > 1` using the same sanitizer discipline as core's slug — lowercase `[a-z0-9._-]`); thread the owning `capture_dir` from `watcher.rs` → the enqueue consumer → `build_package_for_file`; `verify_and_register` passes `DeviceCapability::Perseus` and drops the `set_role`/primary logic; `device_name()` → `config.device_name.clone().unwrap_or_else(|| default_device_name("perseus", &node_id_hex))`; add `targets`/`device_name` to `Config` + template + `validate_ready` (require ≥1 target instead of a primary).

- [ ] **Step 4: Run** — new tests + `cargo test -p perseus` + `cargo build -p perseus` green, warning-free.

- [ ] **Step 5: Commit**

```bash
git add crates/perseus
git commit -m "feat(perseus): register as perseus capability; hostname name; targets config; capture-dir-relative rel_path"
```

---

### Task 7: Perseus — multi-target send + web targets & name editors

Give Perseus one engine per configured target and enqueue each package to every target; add the web-page editors for `targets` and device name (mirroring the capture-dirs editor pattern). Targets changes apply restart-to-apply (engine-bound), like capture-dirs.

**Files:**
- Modify: `crates/perseus/src/run.rs` (`Agent` holds `HashMap<NodeId, Arc<SyncEngineHandle>>`; resolve each target name/id → `NodeId`; enqueue each built package to every engine)
- Modify: `crates/perseus/src/supervisor.rs` (restart-to-apply when `targets` changes — extend the `running_dirs != configured` diff to also compare targets, or track `running_targets`)
- Modify: `crates/perseus/src/config_edit.rs` (`apply_targets_edit`, `apply_device_name_edit` — `toml_edit` write-back + `validate` + atomic rename, mirroring `apply_capture_dirs_edit`)
- Modify: `crates/perseus/src/web.rs` (`GET/PUT /api/targets`, `GET/PUT /api/device-name` routes + DTOs; `WebState` restart-pending for targets)
- Modify: `crates/perseus/src/web/account_api.rs` (surface device name in `AccountDto` if editing lives in the account card) and/or `src/web/index.html` (targets list editor + name field)
- Modify: `crates/perseus/src/web/index.html` (targets editor section mirroring capture-dirs; device-name input in the account card)
- Test: `crates/perseus/src/config_edit.rs` inline (`apply_targets_edit` round-trip); `crates/perseus/src/run.rs` inline (resolve N targets → N engines)

**Interfaces:**
- Produces:
  ```rust
  // config_edit.rs
  pub fn apply_targets_edit(config_path: &Path, targets: &[String]) -> Result<Config>;
  pub fn apply_device_name_edit(config_path: &Path, name: &str) -> Result<Config>;
  // run.rs — Agent send fans out
  // for each configured target resolved to NodeId: engine.enqueue_package(&pkg_dir)
  ```
- Consumes: `SyncEngine::spawn(store, transport, peer)` per target (unchanged core engine — one peer each); `resolve_dest_node`-equivalent resolution from the pairing cache / hub device list by name or id.

- [ ] **Step 1: Write failing test**

```rust
// config_edit.rs tests
#[test]
fn apply_targets_edit_writes_and_reparses() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_min_config(dir.path()); // capture_dirs + [account]
    let cfg = apply_targets_edit(&path, &["studio-mac".into(), "nas-01".into()]).unwrap();
    assert_eq!(cfg.targets, vec!["studio-mac".to_string(), "nas-01".to_string()]);
    // re-load from disk to prove the write-back persisted + re-parses
    let reloaded = Config::load_lenient(&path).unwrap();
    assert_eq!(reloaded.targets.len(), 2);
}
```

- [ ] **Step 2: Run, confirm fail** — `cargo test -p perseus --lib apply_targets_edit_writes_and_reparses` → FAIL.

- [ ] **Step 3: Implement** — `apply_targets_edit`/`apply_device_name_edit` (`toml_edit`, atomic rename, mirror `apply_capture_dirs_edit`); `run.rs` resolves every target to a `NodeId` (by id, else by name via the device-name cache / hub list), spawns/holds an engine per target, and the enqueue consumer loops the built package over all engines (a package that fails to one target still goes to the others — log per-target, never fail the whole send); `supervisor.rs` restarts the agent when `targets` changes (extend the running-set diff); web routes + DTOs + `index.html` editors (targets list like capture-dirs; device-name input in the account card calling `PUT /api/device-name` → after save, ring `supervisor_wake` so re-registration picks it up, or call the account rename path directly).

  > Multi-target failure policy (spec §8): each target is independent. Build the package once, enqueue to each engine; a per-target dial/enqueue failure is logged (`warn!`) and does not abort the others. Record this in the report.

- [ ] **Step 4: Run** — new tests + `cargo test -p perseus` + `cargo build -p perseus` green, warning-free.

- [ ] **Step 5: Commit**

```bash
git add crates/perseus
git commit -m "feat(perseus): multi-target send (one engine per target) + web targets & device-name editors"
```

---

## Self-Review

**Spec coverage:**
- §3 capability model → Task 1 (enum, verify, `/devices`) + Task 6 (Perseus registers `perseus`).
- §4 allow-list = account → **already landed in Plan 2A** (not re-done here).
- §5 node names (unique, hostname default, `PATCH` edit) → Task 4 (hostname + `PATCH` client/command/route) + Task 5 (app editor) + Task 7 (Perseus editor). Uniqueness (409) surfaced to the UI in Tasks 5/7.
- §6 mirror landing → **landed in Plan 2B**; Task 6 supplies the capture-dir-relative `rel_path` the mirror consumes + the multi-root label (§6 edge case).
- §7 dedup offer/want → **Plan 3, out of scope here** (explicitly deferred).
- §8 target selection → Task 3 (explicit-target primitive) + Task 7 (Perseus `targets` + fan-out).
- §13 decision 1 (autostart=signed-in) → **Plan 2A**. Decision 2 (no app auto-send) → Task 2. Decision 3 (drop role selector) → Task 5. Decision 4 (inert local state) → Task 2 removes the reads/writes.

**Placeholder scan:** deletion-heavy tasks (2) name exact symbols + files; new logic (Tasks 1,3,4,6,7 code blocks; Task 5 UI) is specified. Path math and name fallback are factored into unit-testable pure fns (`compute_rel_path`, `fallback_name`) so tests don't mutate host state. Task 5 honestly flags the frontend-test-runner uncertainty rather than assuming one.

**Type consistency:** `DeviceCapability` (Task 1) is the single capability type used by client `verify` (Task 1), `resolve_dest_node`'s Perseus-exclusion (Task 3), and Perseus registration (Task 6). `default_device_name(prefix, node_id_hex)` (Task 4) is called by both app `sign_in_verify` (Task 4) and Perseus `device_name()` (Task 6). `SyncSenderRuntime` per-peer map (Task 3) is consumed by the tauri/web `enqueue_sync_selection` (Task 3) and mirrored by Perseus's per-target engines (Task 7).

**Ordering rationale:** 1 (types) → 2 (remove old paths, compiler-clean) → 3 (new send primitive) → 4 (naming/PATCH plumbing) → 5 (app UI, needs 1+4) → 6 (Perseus registration/rel_path, needs 1+4) → 7 (Perseus multi-target/web, needs 3+6). Each task ends green on its crate(s); `tsc` is red only between Task 1 and Task 5 (noted in both commit bodies).

---

## Execution Handoff

Execute with **superpowers:subagent-driven-development** (owner's standing choice for this cycle): fresh implementer per task (rust-engineer + opus for core/perseus, frontend-dev + opus for Task 5), a spec+quality review after each, a broad whole-branch review at the end. Ledger: `.superpowers/sdd/progress-2c.md`. Do **not** deploy — Plan 2 (2A+2B+2C) ships jointly with the hub in a later, owner-gated step.

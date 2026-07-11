# Sync Phase 2A — allow-list = account + receiver autostart = signed-in

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move two core sync-gating decisions to the mesh model: the receiver authorizes **any device in my account** (not just a capture device paired to me), and a **signed-in** Athenaeum node autostarts its receiver (not only a `role == Primary` one).

**Architecture:** Both changes live in `crates/athenaeum-core/src/api/sync.rs` (host-agnostic; both desktop and web route through it). They are pure gating changes — no enum rename, no UI, no send-path work — so the core crate stays green and both are unit-testable. The bigger capability-model rename, `set_role` removal, mirror landing, and explicit-target send are separate plans (2B/2C).

**Tech Stack:** Rust, `athenaeum-core`. Tests: `cargo test -p athenaeum-core`.

**Spec:** `docs/superpowers/specs/2026-07-11-sync-model-phase1-design.md` §4 (allow-list = account) and §13 decision 1 (autostart = signed-in). Depends on the merged hub (Plan 1) which returns `capability` and drops `role`/`peerDeviceId` from `GET /devices`.

## Global Constraints

- All in `crates/athenaeum-core/src/api/sync.rs`; follow existing patterns there.
- The receiver's `PeerAuthorizer` enforcement and the `SYNC_AUTHORIZED_PEERS` setting are unchanged — only the SET of authorized hexes changes (Task 1) and the autostart gate (Task 2).
- Fail-closed semantics preserved: an empty cache still authorizes nobody.
- `AccountDevice` (from `crate::account`) has fields `id, name, pubkey (String, base64), role (Option<DeviceRole>), peer_device_id (Option<String>), created_at, last_seen_at`. The new hub omits `role`/`peer_device_id`, so serde leaves them `None` — Task 1 must NOT depend on them.
- Commits: author is the repo git user (`eg013ra1n`); **no Claude co-author/footer**.
- Run `cargo test -p athenaeum-core` green before each commit; keep `cargo build -p athenaeum-core` warning-free (remove any import/fn left unused by the change).

---

### Task 1: Allow-list = every device in my account

`refresh_authorized_peers` currently caches only capture devices whose `peer_device_id` is this node — a pairing that no longer exists. Cache **every** account device's pubkey instead.

**Files:**
- Modify: `crates/athenaeum-core/src/api/sync.rs` (`refresh_authorized_peers`; extract the filter to a pure fn)
- Test: `crates/athenaeum-core/src/api/sync.rs` (inline `#[cfg(test)]`)

**Interfaces:**
- Produces: `fn account_peer_hexes(devices: &[crate::account::AccountDevice]) -> Vec<String>` — the 64-char lowercase hex node id of every device whose base64 `pubkey` decodes to 32 bytes (order preserved, undecodable dropped). Consumed by `refresh_authorized_peers` and any later plan needing the account allow-list.

- [ ] **Step 1: Write the failing test**

Add near the other `api::sync` tests (inline `mod tests`). If a `use` for `AccountDevice` is missing, add `use crate::account::AccountDevice;` in the test module:

```rust
#[test]
fn account_peer_hexes_includes_every_device_regardless_of_role() {
    use base64::Engine;
    let b64 = |bytes: [u8; 32]| base64::engine::general_purpose::STANDARD.encode(bytes);
    let dev = |seed: u8, role: Option<crate::account::DeviceRole>, peer: Option<&str>| crate::account::AccountDevice {
        id: format!("dev-{seed}"),
        name: Some(format!("n{seed}")),
        pubkey: b64([seed; 32]),
        role,
        peer_device_id: peer.map(str::to_string),
        created_at: "2026-07-11T00:00:00Z".into(),
        last_seen_at: None,
    };
    let devices = vec![
        dev(1, None, None),                                   // no role (new hub)
        dev(2, Some(crate::account::DeviceRole::Capture), Some("dev-9")), // unpaired-to-me capture
        dev(3, Some(crate::account::DeviceRole::Primary), None),
    ];
    let hexes = account_peer_hexes(&devices);
    assert_eq!(hexes.len(), 3, "every account device is authorized, not just paired captures");
    assert!(hexes.contains(&"01".repeat(32)));
    assert!(hexes.contains(&"02".repeat(32)));
    assert!(hexes.contains(&"03".repeat(32)));
}
```

- [ ] **Step 2: Run it, confirm it fails**

Run: `cargo test -p athenaeum-core --lib account_peer_hexes_includes_every_device_regardless_of_role`
Expected: FAIL — `account_peer_hexes` does not exist (compile error).

- [ ] **Step 3: Add `account_peer_hexes` and use it in `refresh_authorized_peers`**

Add the pure helper (near `pubkey_b64_to_hex`):

```rust
/// The hex node ids of every device in the account — the receiver's allow-list
/// in the mesh model (finding H1, updated for sync Phase 1): any device in my
/// account is trusted. Undecodable pubkeys are skipped.
fn account_peer_hexes(devices: &[crate::account::AccountDevice]) -> Vec<String> {
    devices.iter().filter_map(|d| pubkey_b64_to_hex(&d.pubkey)).collect()
}
```

In `refresh_authorized_peers`, replace the `let hexes = devices.iter().filter(|d| d.role == ... && d.peer_device_id ...).filter_map(...).collect();` block with:

```rust
    let hexes = account_peer_hexes(&devices);
```

The `self_device_id(ctx)` fetch earlier in `refresh_authorized_peers` (used only for the removed `peer_device_id == self_id` filter) is now unused — delete that `let Some(self_id) = ... else { return };` line. If the `self_device_id` fn becomes unused anywhere else (grep `self_device_id`), delete the fn too; otherwise leave it.

- [ ] **Step 4: Run the test + the full core suite**

Run: `cargo test -p athenaeum-core --lib account_peer_hexes_includes_every_device_regardless_of_role`
Expected: PASS.
Run: `cargo test -p athenaeum-core` and `cargo build -p athenaeum-core`
Expected: all green, no unused-import/fn warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/api/sync.rs
git commit -m "feat(sync): authorize any account device on the receiver (drop capture-paired filter)"
```

---

### Task 2: Receiver autostart on any signed-in node (drop the role gate)

`autostart_if_enabled` starts the receiver only for the dev flag OR a `role == Primary` device. In the mesh model every signed-in Athenaeum node is a full peer, so it should start on the dev flag OR simply being signed in.

**Files:**
- Modify: `crates/athenaeum-core/src/api/sync.rs` (`account_primary_ready` → `account_signed_in`; `autostart_gate`; the call in `autostart_if_enabled`)
- Test: `crates/athenaeum-core/src/api/sync.rs` (inline `#[cfg(test)]`)

**Interfaces:**
- Consumes: `keys::ACCOUNT_DEVICE_ID` (the settings key written on sign-in, cleared on sign-out).
- Produces: `fn account_signed_in(ctx: &ServiceContext) -> Result<bool, ApiError>` (true iff `ACCOUNT_DEVICE_ID` is present) and `fn autostart_gate(dev: bool, signed_in: bool) -> bool` (`dev || signed_in`).

- [ ] **Step 1: Write the failing test**

The existing `autostart_gate` matrix test (search `autostart_gate` in the test module) asserts the old `dev || account_primary` shape. Add the new signed-in-gate behavior:

```rust
#[test]
fn autostart_gate_starts_for_any_signed_in_node() {
    // dev flag alone starts (unchanged).
    assert!(autostart_gate(true, false));
    // signed in (any Athenaeum node) starts — no role required.
    assert!(autostart_gate(false, true));
    // neither → no autostart.
    assert!(!autostart_gate(false, false));
}
```

- [ ] **Step 2: Run it, confirm it fails**

Run: `cargo test -p athenaeum-core --lib autostart_gate_starts_for_any_signed_in_node`
Expected: FAIL — `autostart_gate`'s second parameter still means "account_primary" (a role-gated bool); the signature/semantics compile but the intent test coexists — actually it will PASS by luck since `dev || x` is unchanged. **If it passes, that is fine** — the behavioral change is in the CALLER (`account_signed_in` replacing `account_primary_ready`). Proceed to also add the caller-level test below, which genuinely fails:

```rust
// Signed in with NO role set must now autostart (old code required role==Primary).
#[tokio::test]
async fn autostart_starts_when_signed_in_without_primary_role() {
    use crate::sync::SyncRuntime;
    let ctx = test_ctx_with_account_device_id_but_no_role(); // helper below
    let sync = SyncRuntime::new();
    let started = autostart_if_enabled(&ctx, &sync, std::sync::Arc::new(crate::events::NullEmitter)).await;
    assert!(matches!(started, Ok(true)), "a signed-in node autostarts its receiver regardless of role: {started:?}");
}
```

For the `test_ctx_with_account_device_id_but_no_role` helper: follow the existing autostart tests' `ServiceContext` construction (search the test module for how `autostart_if_enabled` is already tested — reuse that ctx builder, and set `ACCOUNT_DEVICE_ID` via `set_setting` while leaving `ACCOUNT_ROLE` unset). If the existing tests build the ctx inline, mirror that; do not invent a new harness.

- [ ] **Step 3: Run it, confirm it fails**

Run: `cargo test -p athenaeum-core --lib autostart_starts_when_signed_in_without_primary_role`
Expected: FAIL — old `account_primary_ready` requires `role == Primary`, so a role-less signed-in node returns `Ok(false)`.

- [ ] **Step 4: Replace `account_primary_ready` with `account_signed_in`**

Rename `account_primary_ready` to `account_signed_in` and drop the role check:

```rust
/// Local-state-only "is this node signed in" check for [`autostart_if_enabled`]:
/// the persisted `ACCOUNT_DEVICE_ID` the app writes on sign-in / clears on
/// sign-out. Every signed-in Athenaeum node is a full peer (capability
/// `athenaeum`) and runs a receiver — there is no role gate (sync Phase 1).
fn account_signed_in(ctx: &ServiceContext) -> Result<bool, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    Ok(crate::db::get_setting(&conn, keys::ACCOUNT_DEVICE_ID)?
        .filter(|s| !s.is_empty())
        .is_some())
}
```

Update `autostart_if_enabled`: replace `let account_primary = account_primary_ready(ctx)?;` with `let signed_in = account_signed_in(ctx)?;`, `if !autostart_gate(dev, signed_in)`, and the `tracing::debug!(dev, account_primary, ...)` field to `signed_in`. Update `autostart_gate`'s param name/doc from `account_primary` to `signed_in` (body `dev || signed_in` unchanged).

Grep for any other caller of `account_primary_ready` and update it to `account_signed_in`.

- [ ] **Step 5: Run both new tests + the full suite**

Run: `cargo test -p athenaeum-core --lib autostart_gate_starts_for_any_signed_in_node autostart_starts_when_signed_in_without_primary_role`
Expected: PASS.
Run: `cargo test -p athenaeum-core` and `cargo build -p athenaeum-core`
Expected: all green, warning-free (update or remove any existing `account_primary_ready`-named test that no longer matches).

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-core/src/api/sync.rs
git commit -m "feat(sync): any signed-in node autostarts its receiver (drop role==Primary gate)"
```

---

## Self-Review

**Spec coverage:** §4 allow-list = account → Task 1. §13 decision 1 autostart = signed-in → Task 2. Both are the only items in 2A's scope; capability rename / set_role removal / send path / mirror / UI are explicitly 2B/2C (out of scope here). ✅

**Placeholder scan:** Task 2 Step 2's ctx helper is described by pointing at the existing autostart-test harness rather than inventing one — the implementer must reuse the real builder (called out as a NEEDS-existing-pattern, not a TODO). The RED expectation for `autostart_gate_starts_for_any_signed_in_node` is honestly noted as possibly-green (the real RED is the caller test) — no false RED claim.

**Type consistency:** `account_peer_hexes(&[AccountDevice]) -> Vec<String>`, `account_signed_in(&ServiceContext) -> Result<bool, ApiError>`, `autostart_gate(bool, bool) -> bool` are used consistently between definition and call sites.

---

## Execution Handoff

Plan complete. Two options:
1. **Subagent-Driven (recommended)** — fresh subagent per task, review between (`superpowers:subagent-driven-development`).
2. **Inline** — tasks in this session with checkpoints (`superpowers:executing-plans`).

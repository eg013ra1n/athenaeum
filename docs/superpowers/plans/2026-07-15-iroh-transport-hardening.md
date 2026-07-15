# iroh Transport Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remediate the 2026-07-15 iroh audit end-to-end per the approved spec (`docs/superpowers/specs/2026-07-15-iroh-transport-hardening-design.md`): one shared iroh endpoint per process (C1), unified blob store, hub-carried device addresses (H1), bounded relay-map staleness (H2), graceful shutdown + key lock + gate parity (I1/I3/I4), and the minors.

**Architecture:** A new `SharedIrohNode` (core `sharing/iroh/node.rs`) owns the single Endpoint + Router + FsStore and hands out role handles; the existing `IrohTransport` becomes its internal engine room and `SharingTransport` stays the consumer trait. Event demux routes acks by (peer, package id) claims and inbound announces to the registered receiver. Hub work is two lineages: `device-addresses` from hub **main** (prod-deployable now — single-tester env), plus one holder-surface commit on `collab-portal`.

**Tech Stack:** Rust (iroh =1.0.2, iroh-blobs =0.103.0, new dep `fd-lock`), axum + sqlx (hub), no frontend changes.

## Global Constraints

- **Repos/branches:** athenaeum `0.5.0` (continue); athenaeum-hub NEW branch `device-addresses` cut from `main` (9a0076d) + ONE commit on `collab-portal`. Hub migration number: **0011** (0009/0010 are taken by the unmerged collab lineage; sqlx tolerates the gap — verified `db.rs:19-25`).
- **Wire compat is absolute:** postcard variant indices frozen (`sharing/iroh/proto.rs:91`) — NO new wire messages, NO reordering. All H1 data flows over hub HTTP (additive JSON). Old beta.2 devices must interoperate: dialers always FALL BACK to the current our-relay-map hint when a reported address is absent or fails.
- **Loopback test semantics unchanged** — existing suites must pass without weakening; they now exercise the shared node.
- **Never swallow errors; tracing message style** (short phrase + snake_case fields; new field names → spec dictionary update in the same commit).
- **Gates per task:** `cargo build --workspace`, `cargo test -p athenaeum-core`, `cargo test -p perseus`, `cargo build -p perseus --no-default-features`, `npx tsc --noEmit` (should be no-op — no TS surface changes; run to prove it). Hub tasks: `cargo test` in athenaeum-hub (needs local Postgres — `#[sqlx::test]`).
- **Commit identity:** `eg013ra1n <vilen.sharifov@gmail.com>`, never a Claude author/co-author line.
- **Rollout order is binding:** T1→T2→T3→T4 (C1 complete + testable) before T5-T7 (H1) before T8 (H2); T9 last.

## Design decisions resolved during planning (Д1–Д4)

- **Д1 — Early gating stays handler-level, now with proof.** iroh 1.0.2's `Incoming` (`connection.rs:137-213`) and `Accepting` (`connection.rs:584-651`) expose ONLY socket addresses pre-handshake; the authenticated peer id first exists on the completed `Connection` (`remote_id()`, `connection.rs:1127`) — exactly where the gate already runs (`mod.rs:772/:952`). Audit minor #1 resolves to a comment documenting this verification; no code move.
- **Д2 — The node lives on `ServiceContext`** (`iroh_node: Arc<tokio::sync::Mutex<Option<Arc<SharedIrohNode>>>>`), NOT AppState: every core api fn (`ensure_sender_engine`, receiver ensure_started, collab senders) reaches it via `ctx` without threading new params through both hosts. Perseus's `Agent` owns its node directly (no ServiceContext there). `SyncRuntime`/`SyncSenderRuntime` keep their public APIs — they acquire handles from the node instead of constructing transports.
- **Д3 — Role handles carry the tag prefix.** `SharedIrohNode::handle(Role::Recv|Out|Collab)` returns a `SharingTransport` implementor that prefixes tags (`recv/pkg/…`, `out/pkg/…`, `collab/pkg/…` — today's single namer is `package_tag` `mod.rs:78-80`) and whose `start()` sweep deletes ONLY its own prefix (replacing `tags().delete_all()` `mod.rs:451` with a prefixed scan-delete). All handles share the node's single `FsStore` instance (a second `FsStore::load` on the dir would hit the redb lock).
- **Д4 — Ack claims registry.** Engines already peer-bind acks (`engine.rs:1016-1024`) and match by package id (`engine.rs:1028-1031`); the node demux routes `AckReceived{from, package_id}` to the consumer that CLAIMED `(from, package_id)` at announce time (claim added in the handle's `announce`, released on ack/failure/shutdown). Inbound `AnnounceReceived`/`Project*` route to the single registered Recv consumer. An event matching no claim/consumer logs `warn!(kind, peer, "inbound event with no consumer")` and is NOT acked — replacing the silent `Ok(())` drops (`engine.rs:990-994` stay as engine-side defense but become unreachable for misrouted events).

---

### Task 1: SharedIrohNode — endpoint/store/lock/shutdown core

**Files:**
- Create: `crates/athenaeum-core/src/sharing/iroh/node.rs`; Modify: `sharing/iroh/mod.rs` (declare, re-export, expose internals the node needs), `sharing/mod.rs` (trait: add `async fn shutdown(&self)` default no-op), `crates/athenaeum-core/Cargo.toml` (+`fd-lock`)
- Modify: `crates/athenaeum-core/src/account/keys.rs` (lock helper next to `DeviceKey`)

**Interfaces (BINDING for T2-T4, T7-T8):**

```rust
pub enum Role { Recv, Out, Collab }

pub struct SharedIrohNode { /* endpoint, router, store, key lock guard, demux (T2), … */ }

impl SharedIrohNode {
    /// Binds the ONE endpoint for this process: takes the device-key advisory
    /// lock (fd-lock; fail = actionable error naming the other process risk),
    /// builds Endpoint (presets::Minimal + secret + relay_mode + MemoryLookup),
    /// opens the single FsStore at <sync_dir>/blobs (GC enabled once), mounts
    /// Router with both ALPNs ONCE, spawns the home_relay_status watcher
    /// (info!/warn! on transitions — iroh endpoint.rs:1384).
    pub async fn bind(sync_dir: &Path, relay_mode: RelayMode) -> Result<Arc<Self>>;
    pub fn node_id(&self) -> NodeId;
    pub fn endpoint_addr(&self) -> EndpointAddr;          // for H1 reporting (T7)
    pub fn relay_urls(&self) -> Vec<String>;              // what it was built with
    /// Role handle implementing SharingTransport (announce/request/fetch/serve/
    /// events/add_peer per role; tag prefix per Д3; prefix-scoped start sweep).
    pub fn handle(self: &Arc<Self>, role: Role) -> Arc<dyn SharingTransport>;
    /// Router::shutdown().await → store shutdown → endpoint.close() bounded 5s.
    /// Idempotent. Releases the key lock.
    pub async fn shutdown(&self);
}
```

Implementation notes: reuse `IrohTransport`'s existing construction body (`mod.rs:184-278`) — the node IS a refactored `IrohTransport` plus lock/roles/demux; keep `IrohTransport::new` alive for the loopback/test paths until T3 migrates them, then it becomes `pub(crate)` plumbing. Key lock: `fd_lock::RwLock` on `<sync_dir>/device_key` (the file `DeviceKey::load_or_create` manages, `keys.rs:41`), held for node lifetime; second locker → error "device key is in use by another process (copied key or double launch) — each install needs its own identity". Prefix sweep: `tags().list()` filter by `"{prefix}/pkg/"` → delete each (iroh-blobs `tags()` API; `delete_all` `mod.rs:451` retired). Keep the 2026-07-14 diagnostics intact. Д1 comment lands here (gate placement proof).

- [ ] **Step 1: failing tests** (loopback, in `node.rs` test mod): (a) two `SharedIrohNode::bind` on the same sync_dir → second fails with the lock message; (b) prefix sweep — seed tags `out/pkg/a`, `recv/pkg/b`, `collab/pkg/c` in the store, start the Out handle, assert only `out/pkg/a` deleted; (c) bind→shutdown→re-bind same dir succeeds (lock released, store closed cleanly); (d) `handle(Role::X)` twice returns working transports sharing one store (import via Out, read via Recv store handle).
- [ ] **Step 2:** implement node + lock + role handles + prefixed sweep + trait `shutdown`; focused tests green.
- [ ] **Step 3:** full gates + commit `feat(sync): SharedIrohNode — single endpoint/store per process, device-key lock, role-scoped tag sweeps`.

### Task 2: Event demux + control-connection pool

**Files:** Modify: `crates/athenaeum-core/src/sharing/iroh/node.rs` + `mod.rs` (event fan-in), `sharing/types.rs` (no wire changes — in-process only)

**Behaviors (Д4 binding):**
1. The node owns the single transport event stream; `handle(Role::Recv).events()` yields inbound `AnnounceReceived`/`Request*`/`Project*`; ack claims: the Out/Collab handle's `announce(peer, pkg)` registers `(peer, package_id)` → that handle's events channel receives the matching `AckReceived`; claim released on delivery/failure/drop.
2. Orphan events (no claim/consumer): `warn!(kind, peer, "inbound event with no consumer")`, event dropped WITHOUT the transport-level delivery ack (`mod.rs:919-926` path) so the sender retries instead of losing the message silently.
3. Per-peer pooled control connection replacing connect-per-message (`send_control` `mod.rs:376-392`, `send_request` `mod.rs:415-435`): pool keyed by NodeId, idle-close 60s, ack-await semantics unchanged verbatim; pool entry invalidated on send error (next send re-dials). Path-diagnostics watcher spawns once per pooled connection (not per message).
4. Remove the redundant double store-shutdown in the old test-only path (`mod.rs:342-348`).

- [ ] **Step 1: failing tests** (loopback): (a) two Out-handle engines to DIFFERENT peers announce concurrently; acks route to the right claimant (assert both complete, no cross-talk); (b) same package id announced to TWO peers (Perseus multi-dest shape) — each ack completes only its own claim; (c) an announce arriving with no Recv consumer registered → log-capture asserts the orphan warn AND the sender side times out (no silent ack); (d) two sequential `send_control` to one peer reuse a pooled connection (assert via connection-count introspection or the established-path log firing once).
- [ ] **Step 2:** implement demux + pool; focused green.
- [ ] **Step 3:** full gates + commit `feat(sync): shared-node event demux ((peer,package) ack claims) + pooled control connections`.

### Task 3: Migrate app consumers to the shared node

**Files:** Modify: `crates/athenaeum-core/src/services/mod.rs` (Д2 field), `api/sync.rs` (`ensure_sender_engine` :790-846, `autostart_if_enabled` :455, `get_pairing_ticket`/ensure_started :506), `sync/receiver.rs` (:803-858 build path), `api/collab_exchange.rs` (`ensure_collab_sender_engine` :395-440, `download_project_package` :885/:982-993), old-store cleanup, `crates/athenaeum-tauri/src/lib.rs` + `crates/athenaeum-web/src/main.rs` (graceful shutdown wiring)

**Behaviors:**
1. `ctx.iroh_node` lazy-bind on first need (any of the five call sites); all three constructors (`api/sync.rs:824`, `collab_exchange.rs:424`, `receiver.rs:841`) become `node.handle(Role::Out|Collab|Recv)`. `SyncRuntime`/`SyncSenderRuntime` public APIs unchanged.
2. Unified store: receiver's dir (`<sync>/blobs`) is the node store; first bind deletes orphaned `blobs_out`/`blobs_collab` dirs (after node up, `info!` per dir).
3. Shutdown wiring (I1): Tauri — switch to `.build(ctx)?.run(|handle, event| …)` handling `RunEvent::ExitRequested`/`Exit` → block_on bounded `node.shutdown()`; web — `axum::serve(...).with_graceful_shutdown(ctrl_c+SIGTERM)` (`main.rs:357`) → `node.shutdown()` after serve returns. Best-effort, bounded 5s.
4. The stale role-gating comment `api/sync.rs:816-823` is rewritten to describe the shared node.

- [ ] **Step 1:** migrate core call sites, keep every existing loopback/integration test green (they pin the semantics — `cargo test -p athenaeum-core` is the failing-test set for this task; no new tests except a ctx lazy-bind unit test).
- [ ] **Step 2:** host shutdown wiring both hosts; manual verify: `cargo run -p athenaeum-web` + Ctrl-C logs the node shutdown line.
- [ ] **Step 3:** full gates + commit `refactor(sync): app rides the SharedIrohNode — one endpoint per process, unified store, graceful shutdown`.

### Task 4: Perseus on the shared node + gate parity

**Files:** Modify: `crates/perseus/src/run.rs` (:449-486 per-target transports → one node + Out handles; `Agent::shutdown` :862-884 adds `node.shutdown().await`), `crates/perseus/src/account.rs` (:381-385 I3 parity)

**Behaviors:**
1. One `SharedIrohNode::bind` per Agent; per-target engines get `node.handle(Role::Out)` each (engines stay one-per-peer; demux Д4 disambiguates); ticket path still `add_peer_ticket`.
2. I3: `allow_default_relays` effective ONLY when signed out (mirror `api/sync.rs:396` exactly); signed-in + no relays ⇒ the existing loud `bail!` (`account.rs:386-392`). Update the config doc comment.
3. `Agent::shutdown`: after engines shut (`run.rs:882`), `node.shutdown().await`.

- [ ] **Step 1:** failing test — Perseus loopback multi-target test asserting both targets receive from ONE node (existing `start_with_transports` seams keep injection paths working); I3 unit test: signed-in + flag true + empty relays ⇒ Err (not Default).
- [ ] **Step 2:** implement; `cargo test -p perseus` (165+) green.
- [ ] **Step 3:** full gates + commit `refactor(perseus): single shared iroh node across targets; signed-in agents never ride public relays`.

### Task 5: Hub — device address storage + API (branch `device-addresses` from main)

**Repo: athenaeum-hub.** Files: Create `migrations/0011_device_endpoint_addr.sql`; Modify `src/routes/devices.rs`, `src/routes/mod.rs` (route), `tests/device_registry.rs` (+ `tests/common/mod.rs` `put` helper)

**Interfaces:**
- Migration 0011: `ALTER TABLE devices ADD COLUMN IF NOT EXISTS endpoint_addr jsonb;` (nullable; sqlx `serde_json::Value` pattern per collab-lineage precedent).
- `PUT /api/v1/devices/self/address` (protected router, `Extension<AuthDevice>` → `auth.device_id`): body camelCase `{ homeRelayUrl: String|null, directAddrs: [String], reportedAt: String }` — handler stamps `reportedAt` server-side (`now()`), validates `homeRelayUrl` parses as URL and `directAddrs` as socket addrs, caps `directAddrs` at 16; upserts `devices.endpoint_addr`. 200 empty body. `#[tracing::instrument(skip_all)]`, `ApiError` shapes.
- `DeviceView` (`devices.rs:39-47`) + `DEVICE_COLUMNS`/`DeviceRow` gain `endpoint_addr: Option<serde_json::Value>` → wire `endpointAddr` (full object — same-account surface, direct addrs allowed per spec §3 privacy rule).

- [ ] **Step 1: failing tests** (`#[sqlx::test]`): PUT stores + GET /devices returns it; PUT with bad relay URL → 400; unauthenticated → 401; device A cannot see... (list is account-scoped already — assert another account's device list omits it by existing scoping test pattern).
- [ ] **Step 2:** implement; hub suite green.
- [ ] **Step 3:** commit `feat(hub): devices report their iroh endpoint address; device list serves it` on `device-addresses`; push branch. (Deploys: owner-relaxed — prod/test deploy happens post-plan, see checklist.)

### Task 6: Hub — collab holders carry relayUrl only (one commit on `collab-portal`)

**Repo: athenaeum-hub, branch `collab-portal`.** Files: `src/routes/announcements.rs` (:228-234 `HolderView`, :281 `HolderRow`, :289 query, :301-307 loop), its tests

**Behavior:** holder query selects `d.endpoint_addr->>'homeRelayUrl' AS relay_url`; `HolderView` gains `relayUrl: Option<String>`. **Privacy rule (binding, spec §3): direct addrs NEVER appear on this surface** — test asserts the serialized holder JSON contains no `directAddrs` even when the device row has them. Note: 0011 is main-lineage; this commit must tolerate the column's absence until merge — guard with `to_regclass`/column check? NO — simpler: cherry-pick migration 0011 onto collab-portal too (same file, same checksum ⇒ sqlx dedupes at merge).

- [ ] **Step 1:** failing test (holder with address row → relayUrl present, no directAddrs leaked; holder without → null).
- [ ] **Step 2:** implement (+ 0011 cherry-pick); hub suite green on the branch.
- [ ] **Step 3:** commit `feat(hub): collab holders expose home relay url (never direct addrs)`; push.

### Task 7: App/Perseus — report own address, dial with peer's

**Files:** Modify: `crates/athenaeum-core/src/account/client.rs` (+`put_device_address`), `account/mod.rs` (`AccountDevice` + `endpoint_addr: Option<EndpointAddrReport>` with `#[serde(default)]` — additive, old hubs omit it), `sharing/iroh/node.rs` (report trigger), `api/sync.rs` (:841 dial preference), `api/collab_exchange.rs` (:992 + holder consumption, `sync/receiver.rs` blob-pull sites :479/:633 via cached device list), `crates/perseus/src/account.rs`+`run.rs` (:481 preference; reporting)

**Behaviors:**
1. Reporting: on node bind and on endpoint-addr change (debounced 30s; the 2026-07-14 watchers signal), PUT own `{homeRelayUrl, directAddrs}` (from `node.endpoint_addr()`) — fire-and-forget task, `warn!` on failure, never blocks transport start.
2. Dial preference: a shared helper `peer_dial_addr(reported: Option<&EndpointAddrReport>, peer, our_relays) -> EndpointAddr` — reported (relay + direct addrs same-account; relay-only cross-account/holders) when present; ALWAYS also `add_peer` the our-map fallback on dial failure of the reported addr (belt-and-braces, spec §3). Call sites: `ensure_sender_engine` (peer's `AccountDevice.endpoint_addr` — already fetched in `resolve_dest_node`'s `list_devices` :750), collab download holders (`relayUrl`), Perseus `run.rs:481` (`resolve_all_target_peers` list_devices carries it; cache it in `PairingCache`).
3. Receiver blob pull (I2): before `fetch(from,…)` at :479/:633, `add_peer` for `from` using the account device list cache when a matching pubkey is found (else our-map hint — current behavior, now explicit).

- [ ] **Step 1: failing tests**: wiremock hub serving `endpointAddr` → `ensure_sender_engine` add_peer receives the peer's relay (assert via node lookup introspection); absent field → fallback hint (compat test with old-hub JSON, no `endpointAddr` key); holder relayUrl consumed in collab download hint; Perseus cache round-trip.
- [ ] **Step 2:** implement app + Perseus; focused green.
- [ ] **Step 3:** full gates + commit `feat(sync): devices report endpoint addresses; dialers use the peer's real relay with fallback`.

### Task 8: Relay-map lifecycle (H2)

**Files:** Modify: `crates/athenaeum-core/src/sharing/iroh/node.rs` (refresh task + idle rebuild), `api/sync.rs`/`api/collab_exchange.rs` (activity signal), `sync/engine.rs` (:1159-1185 retry re-resolve hook), `crates/perseus/src/run.rs`/`account.rs` (agent refresh)

**Behaviors:**
1. Node-owned refresh task: hourly (+immediately on sign-in) re-run the existing resolver (`resolve_relay_mode` app / `resolve_relays` Perseus, injected as a callback so the node stays hub-agnostic); on CHANGED url set → `info!` + mark pending-rebuild; rebuild = bounded `shutdown()` + re-`bind` + re-register consumers, executed only when idle (no active claims, no serves, no in-flight fetch — the demux registry IS the idle signal); `warn!` if deferred >6h then rebuild at next quiet instant.
2. Engine retry hook: `spawn_inner` gains an optional `addr_refresher: Option<Arc<dyn Fn(NodeId) -> BoxFuture<Option<EndpointAddr>> + Send + Sync>>`; `handle_timeouts` (:1159) awaits it before re-`attempt` and re-`add_peer`s on Some. App wires it to the T7 preference helper (fresh `list_devices` + fresh relays); Perseus likewise.
3. Consumer re-registration after rebuild is transparent to `SyncRuntime`/engines (handles hold a node reference that swaps internals behind a lock — handles survive rebuild).

- [ ] **Step 1: failing tests**: (a) refresh callback returning a changed set triggers rebuild when idle (loopback: node id STABLE across rebuild, store intact, handles still work); (b) rebuild deferred while a claim is active, executes after release; (c) retry path calls the refresher (mock counts) and re-adds the address.
- [ ] **Step 2:** implement; focused green.
- [ ] **Step 3:** full gates + commit `feat(sync): hourly relay-map refresh with idle node rebuild; retries re-resolve peer addresses`.

### Task 9: Minors + live-verification tooling

**Files:** Modify: `crates/athenaeum-core/src/api/collab_exchange.rs` (:1001-1019 holder probe), `sync/models.rs` (:118-127 `OutboundRow.last_error`) + its store write sites + `crates/perseus/src/web.rs` (:590-612 render), `examples/relay_check.rs` (`--paths` mode), Create `crates/athenaeum-core/src/api/relay_live_tests.rs` (`#[ignore]` + `#[cfg(unix)]`)

**Behaviors:**
1. Holder probe: short (5s) control connect per holder BEFORE the 90s blob poll; failure recorded per holder with class `offline | refused | relay_unreachable` into the transfer-history detail (existing history row detail field — read it first); next holder immediately.
2. `OutboundRow.last_error: Option<String>` (store schema — guarded ALTER per house pattern), written on each failed attempt (`engine.rs` failure sites), cleared on success; Perseus web page renders it beside attempts.
3. `relay_check --paths`: after home-relay handshake, print reported-vs-actual home relay (fetch own hub device row when signed in).
4. `relay_live_tests.rs`: `#[ignore]`d test binding two nodes with the SAME key against a real relay URL from env (`ATHENAEUM_TEST_RELAY`) asserting the older connection observes the eviction (home_relay_status transition) — the C1 regression canary, owner-run against test-relay.

- [ ] **Step 1:** failing tests where testable (last_error round-trip; probe classification unit with loopback refusal); implement all four.
- [ ] **Step 2:** full gates + commit `feat(sync): holder probing, per-package last_error, relay live-check tooling`.

## Security requirements (bind all tasks)

- S1: direct addresses NEVER cross accounts (holders = relayUrl only; hub test T6 enforces; app never forwards them).
- S2: the device-key lock failure message must not print key material; lock file is the key file itself (no new secret-adjacent files).
- S3: PUT address validates/normalizes inputs (URL parse, addr parse, count cap) — hub never stores unvalidated strings served back to other devices.
- S4: gate placement unchanged (Д1); ConnectGate coverage on both ALPNs must survive the node refactor (existing gate tests keep passing).
- S5: reported addresses are hints only — content-hash verification and peer-authz (2026-07-11 audit fixes) remain the trust boundary; a malicious address report can misdirect a dial but never authenticate.

## Post-plan checklist (not tasks)

- Deploy hub `device-addresses` to prod + a `collab-portal`(+0011) artifact to test-hub when T5/T6 land (owner-relaxed env); astronet `deploy_athenaeum_hub.yml` with `hub_artifact_ref`.
- Owner acceptance: multi-PC dev smoke re-run after T4 (C1 fixed) and again after T7 (real addresses) — watch `conn_type`/home-relay lines; then the deferred slice-4 live smoke.
- Prod relay map stays at 4 relays during the cycle (owner decision); expected failure signature documented in spec §8.
- Carry-forward: audit minors resolved here close the 2026-07-15 audit; remaining sync ledger follow-ups unchanged.

## Self-review notes

- Wire compat: zero proto.rs changes anywhere in the plan (Д4 demux is in-process; H1 rides hub HTTP; postcard indices untouched).
- `AccountDevice.endpoint_addr` uses `#[serde(default)]` — old hub responses (no field) deserialize fine on new apps; new hub responses are additive JSON old apps ignore. Bidirectional compat proven by T7 Step-1 compat test.
- The engine keeps its own `from != self.peer` ack guard (defense in depth under the demux).
- T3 deliberately adds almost no new tests: the existing loopback suites ARE the regression net for the migration; new behavior (lock, sweep scoping, demux, pool) got its tests in T1/T2.
- Hub migration 0011 appears on BOTH hub lineages with an identical file (same checksum) — sqlx treats it as one version at merge; divergence would be a checksum error, so T6 must cherry-pick, never rewrite.
- `resolve_dest_node` already fetches the device list (`api/sync.rs:750`) — T7's dial preference adds no extra hub round-trip on the send path.

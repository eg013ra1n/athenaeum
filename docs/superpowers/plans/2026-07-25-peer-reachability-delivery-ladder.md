# D1 — Peer Reachability and the Delivery Ladder — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A transfer whose peer went away resumes the moment that peer is reachable again — because the peer says so, or because a flat retry finds it — instead of waiting out a backoff ladder derived from our own failure count.

**Architecture:** A receiver announces itself to its authorized account devices with a new postcard control message on two edges (going online, own relay reconnect). The sender's transport hands that to a host hook, which — gated on local facts only — makes sure an engine exists for that peer and tells it the peer is present; the engine clears its in-memory absence mark and un-parks the packages waiting on it. Behind that signal, the retry schedule learns the difference between "the peer is absent" (flat, no escalation, one probing package per peer) and "the peer refuses us / we are broken" (escalating, as today), and `serve` stops re-importing an already-served package on every attempt.

**Tech Stack:** Rust (`athenaeum-core`), postcard wire, iroh 1.0.2 control ALPN, React/TS for one status string.

**Spec:** `docs/superpowers/specs/2026-07-25-peer-reachability-delivery-ladder-design.md`. **Audit:** `docs/superpowers/research/2026-07-25-delivery-model-audit.md`. Read the spec before starting any task.

## Global Constraints

- Branch: `0.5.0`. Commit as `eg013ra1n` — NEVER add a Claude co-author footer.
- **Wire rule:** `Msg::Presence` is appended at the **END** of the enum. Postcard keys variants by declaration index; no existing variant may be reordered, retyped or removed, and `SYNC_ALPN` is NOT bumped (additive-only). Golden bytes are re-pinned in the same task, never silently.
- Perseus links this code: `cargo build -p perseus --no-default-features` must keep passing in every task.
- Logging: message = short stable phrase, data in snake_case fields; new field names come from the dictionary in `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` or are added there in the same task. Zero `println!`.
- `anyhow::Result` inside core; `.map_err(|e| e.to_string())` only at the command boundary. Never swallow an error silently.
- No new DB table, no migration, no new Tauri command, no Axum route — so the two-backend rule does not apply to this plan. If a task appears to need one, stop and re-read the spec.
- Gates for every task: `cargo build --workspace --all-targets`, that task's tests, and `cargo build -p perseus --no-default-features`. Frontend task adds `npx tsc --noEmit`.
- New Rust files must be `rustfmt`-clean (`rustfmt --edition 2021 --check <file>`). Pre-existing files in this tree are NOT rustfmt-clean — do not reformat them wholesale; match the style of the code you touch.

## File Structure

| File | Responsibility | Task |
| ---- | ---- | ---- |
| `crates/athenaeum-core/src/sharing/iroh/proto.rs` | `Msg::Presence` variant | 1 |
| `crates/athenaeum-core/src/sharing/wire_golden_tests.rs` | frozen byte pin for the new variant | 1 |
| `crates/athenaeum-core/src/sharing/mod.rs` | `SharingTransport::send_presence` (defaulted) | 2 |
| `crates/athenaeum-core/src/sharing/iroh/mod.rs` | presence hook slot, accept-loop dispatch, `build_router` param | 2 |
| `crates/athenaeum-core/src/sharing/iroh/node.rs` | `set_presence_hook`, `send_presence` (fresh connection), idempotent `role_serve` | 2, 5 |
| `crates/athenaeum-core/src/sync/engine.rs` | class-aware backoff, `PeerReachability`, coalescing, `Command::PeerPresent` | 3, 4 |
| `crates/athenaeum-core/src/sync/sender.rs` | `SyncSenderRuntime::kick_peer` | 6 |
| `crates/athenaeum-core/src/api/sync.rs` | presence hook wiring + gates, beacon fan-out on the two edges | 6 |
| `crates/athenaeum-core/src/sync/status.rs` + `src/components/transfers/…` | honest per-row text | 7 |

---

### Task 1: `Msg::Presence` on the wire

**Files:**
- Modify: `crates/athenaeum-core/src/sharing/iroh/proto.rs` (append the variant at the end of `enum Msg`)
- Test: `crates/athenaeum-core/src/sharing/wire_golden_tests.rs`

**Interfaces:**
- Produces: `Msg::Presence` — a unit variant, no payload. Task 2 encodes and decodes it.

- [x] **Step 1: Write the failing golden test**

Add to `crates/athenaeum-core/src/sharing/wire_golden_tests.rs`, following the file's existing pin style:

```rust
/// `Msg::Presence` is the LAST variant, so its postcard encoding is its
/// declaration index and nothing else. Pinning it here is what makes a future
/// reordering fail the build instead of silently re-keying every variant.
#[test]
fn presence_golden_bytes() {
    let bytes = Msg::Presence.encode().expect("encode presence");
    assert_eq!(bytes, vec![10], "presence is variant index 10 with no payload");
    assert!(
        matches!(Msg::decode(&bytes).expect("decode presence"), Msg::Presence),
        "presence round-trips"
    );
}
```

- [x] **Step 2: Run it and watch it fail**

Run: `cargo test -q -p athenaeum-core --lib presence_golden_bytes`
Expected: FAIL — `no variant named Presence found for enum Msg`.

- [x] **Step 3: Append the variant**

At the very END of `pub enum Msg` in `proto.rs`, after `Revoke { .. }`:

```rust
    // Peer reachability (D1) — appended AFTER `Revoke` as the LAST variant; every
    // index above stays frozen (same append-only rule as the blocks above). A
    // receiver sends this to its authorized account devices when it comes online,
    // so a sender parked on a retry resumes immediately instead of waiting out its
    // schedule. Carries NO payload: the sender's identity is the authenticated
    // `remote_id` of the connection it arrives on, and anything else would be
    // unverified peer input.
    Presence,
```

- [x] **Step 4: Verify the pin passes and no other pin moved**

Run: `cargo test -q -p athenaeum-core --lib wire_golden`
Expected: PASS, every pre-existing golden test included. **If any OTHER golden test fails, stop** — the variant was not appended at the end.

- [x] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/sharing/iroh/proto.rs crates/athenaeum-core/src/sharing/wire_golden_tests.rs
git commit -m "feat(sync): Msg::Presence wire variant (appended, indices frozen)"
```

---

### Task 2: Sending and receiving presence over iroh

**Files:**
- Modify: `crates/athenaeum-core/src/sharing/mod.rs` (trait method with a default)
- Modify: `crates/athenaeum-core/src/sharing/iroh/mod.rs` (`SharedPresenceHook`, `build_router` parameter, accept-loop arm)
- Modify: `crates/athenaeum-core/src/sharing/iroh/node.rs` (`set_presence_hook`, `send_presence`, pass the slot into `build_router`)
- Test: `crates/athenaeum-core/src/sharing/iroh/tests.rs`

**Interfaces:**
- Consumes: `Msg::Presence` (Task 1).
- Produces:
  - `SharingTransport::send_presence(&self, to: NodeId) -> anyhow::Result<()>` — default `Ok(())`, so no other implementor breaks.
  - `pub type PresenceHook = Arc<dyn Fn(NodeId) + Send + Sync>;`
  - `SharedIrohNode::set_presence_hook(&self, hook: PresenceHook)` — Task 6 installs the host closure here.

- [x] **Step 1: Write the failing transport test**

Add to `crates/athenaeum-core/src/sharing/iroh/tests.rs` (two real endpoints, relay disabled — the file's existing pattern):

```rust
/// A presence beacon reaches the peer's installed hook, carrying the AUTHENTICATED
/// sender id (not anything from the message, which has no payload).
#[tokio::test]
async fn presence_beacon_fires_the_peers_hook() {
    let home = tempfile::tempdir().unwrap();
    let sender = SharedIrohNode::bind(&home.path().join("a"), RelayMode::Disabled).await.unwrap();
    let receiver = SharedIrohNode::bind(&home.path().join("b"), RelayMode::Disabled).await.unwrap();

    let seen: Arc<Mutex<Vec<NodeId>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let seen = Arc::clone(&seen);
        sender.set_presence_hook(Arc::new(move |from| {
            seen.lock().expect("seen mutex poisoned").push(from);
        }));
    }

    // Both sides need an address for each other: relay-disabled endpoints have no
    // discovery, so exchange tickets exactly like the other tests in this file.
    let sender_info = sender.handle(Role::Out).start().await.unwrap();
    let receiver_info = receiver.handle(Role::Recv).start().await.unwrap();
    sender.add_peer_ticket(&receiver_info.pairing_ticket).unwrap();
    receiver.add_peer_ticket(&sender_info.pairing_ticket).unwrap();

    receiver.send_presence(sender.node_id()).await.expect("beacon sends");

    for _ in 0..200 {
        if !seen.lock().unwrap().is_empty() { break; }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        &[receiver.node_id()],
        "the hook fires once with the beacon sender's authenticated id"
    );

    sender.shutdown().await;
    receiver.shutdown().await;
}
```

- [x] **Step 2: Run it and watch it fail**

Run: `cargo test -q -p athenaeum-core --lib presence_beacon_fires_the_peers_hook`
Expected: FAIL — `no method named set_presence_hook` / `send_presence`.

- [x] **Step 3: Add the trait method with a default**

In `crates/athenaeum-core/src/sharing/mod.rs`, inside `trait SharingTransport`, next to `list_in_flight_tags` (the file's existing defaulted-method pattern):

```rust
    /// Tell `to` that we are online and listening (D1 presence beacon).
    ///
    /// Best-effort and fire-and-forget by contract: the caller logs a failure and
    /// moves on, because the receiving side's retry schedule is the fallback. The
    /// default is a no-op so a transport that has no control channel (or a test
    /// double) need not implement it.
    async fn send_presence(&self, _to: NodeId) -> anyhow::Result<()> {
        Ok(())
    }
```

- [x] **Step 4: Add the hook slot and the accept-loop dispatch**

In `crates/athenaeum-core/src/sharing/iroh/mod.rs`, beside `SharedConnectGate`/`SharedResponder`:

```rust
/// Host callback fired when a peer announces its presence (D1). Installed by the
/// host through [`node::SharedIrohNode::set_presence_hook`]; unset until then, so
/// a beacon arriving before the host wires up is dropped at debug rather than
/// queued. Called with the AUTHENTICATED sender id from the connection.
pub type PresenceHook = Arc<dyn Fn(NodeId) + Send + Sync>;

/// The presence-hook slot shared into the control protocol handler.
pub(crate) type SharedPresenceHook = Arc<Mutex<Option<PresenceHook>>>;
```

Add `presence: SharedPresenceHook` as a field of `SyncControlProtocol` and a parameter of `build_router` (both call sites — `IrohTransport::new` passes a permanently-empty slot, `SharedIrohNode::bind` passes its own).

In the accept loop's `match msg`, add an arm BEFORE the fallthrough. Presence is not a package event: it never enters the demux, it fires the hook directly and answers the delivery ack like every other message:

```rust
                Msg::Presence => {
                    let hook = self.presence.lock().expect("presence hook mutex poisoned").clone();
                    match hook {
                        Some(h) => {
                            tracing::debug!(from = %hex32(&from), "peer presence received");
                            h(from);
                        }
                        None => tracing::debug!(
                            from = %hex32(&from),
                            "peer presence dropped; no hook installed"
                        ),
                    }
                    // Ack the delivery like every other control message, then take
                    // the next stream — there is no event to route.
                    let _ = tx.write_all(&[1u8]).await;
                    let _ = tx.finish();
                    continue;
                }
```

- [x] **Step 5: Implement `set_presence_hook` + `send_presence` on the node**

In `crates/athenaeum-core/src/sharing/iroh/node.rs`, add the field `presence: SharedPresenceHook` to `SharedIrohNode` (created in `bind` and passed to `build_router`), plus:

```rust
    /// Install the host's presence callback (D1). Overwrites any previous hook;
    /// left unset, inbound beacons are dropped at debug.
    pub fn set_presence_hook(&self, hook: PresenceHook) {
        *self.presence.lock().expect("presence hook mutex poisoned") = Some(hook);
    }
```

And the send side, on the `SharingTransport` impl for the role handle (delegating to the node):

```rust
/// Beacon timeout. Deliberately far shorter than `CONTROL_SEND_TIMEOUT` (30 s):
/// a beacon fan-out runs at startup across every account device, and a dead peer
/// must not make the live ones wait.
const PRESENCE_SEND_TIMEOUT: Duration = Duration::from_secs(5);
```

```rust
    /// Send a presence beacon on a DEDICATED connection — never the pooled control
    /// channel. A peer running an older build cannot decode this variant, and its
    /// accept loop breaks the whole connection on a decode failure; riding our own
    /// connection means the only casualty is this beacon's own connection, while
    /// the pooled channel that carries announces stays up.
    pub async fn send_presence(&self, to: NodeId) -> Result<()> {
        let target = self.dial_target(to)?;
        let bytes = Msg::Presence.encode()?;
        let endpoint = self.endpoint();
        let send = async {
            let conn = endpoint
                .connect(target, SYNC_ALPN)
                .await
                .context("connect presence channel")?;
            let (mut tx, mut rx) = conn.open_bi().await.context("open presence stream")?;
            tx.write_all(&bytes).await.context("write presence")?;
            tx.finish().context("finish presence stream")?;
            let _ = rx.read_to_end(8).await; // ack is advisory for a beacon
            conn.close(0u32.into(), b"ok");
            anyhow::Ok(())
        };
        tokio::time::timeout(PRESENCE_SEND_TIMEOUT, send)
            .await
            .map_err(|_| anyhow!("presence beacon to {} timed out", hex32(&to)))??;
        Ok(())
    }
```

Wire `RoleHandle`'s `SharingTransport::send_presence` to `self.node.send_presence(to)`.

- [x] **Step 6: Run the test**

Run: `cargo test -q -p athenaeum-core --lib presence_beacon_fires_the_peers_hook`
Expected: PASS.

- [x] **Step 7: Full gates + commit**

```bash
cargo build -q --workspace --all-targets && cargo build -q -p perseus --no-default-features
cargo test -q -p athenaeum-core --lib sharing::
git add crates/athenaeum-core/src/sharing
git commit -m "feat(sync): presence beacon transport — dedicated connection, host hook"
```

---

### Task 3: The ladder learns error classes

**Files:**
- Modify: `crates/athenaeum-core/src/sync/engine.rs` (`retry_backoff`, its two call sites, the contact reset)
- Test: same file's `#[cfg(test)] mod tests` (or `engine_tests.rs` if the module's unit tests live there — follow whichever already holds `retry_backoff` tests)

**Interfaces:**
- Consumes: `crate::sync::diagnostics::ConnectClass` (already exists).
- Produces: `pub fn retry_backoff(base: Duration, rung: u32, class: Option<ConnectClass>) -> Duration` — Task 4 calls it.

- [x] **Step 1: Write the failing tests**

```rust
#[test]
fn absent_classes_retry_flat_and_never_escalate() {
    let base = Duration::from_secs(30);
    // `not_started` is here on purpose: its doc covers "local OR remote endpoint
    // not started", and the remote reading IS an absent peer (it is also what the
    // loopback tests produce). A local one is transient — our own start fires the
    // wake hook long before the flat interval matters.
    for class in [
        ConnectClass::NoRoute,
        ConnectClass::Timeout,
        ConnectClass::RelayUnreachable,
        ConnectClass::NotStarted,
    ] {
        for rung in 0..6 {
            assert_eq!(
                retry_backoff(base, rung, Some(class)),
                base * ABSENT_RETRY_MULTIPLIER,
                "an absent peer is not overloaded — {class:?} at rung {rung} stays flat"
            );
        }
    }
}

#[test]
fn refused_and_local_faults_still_escalate() {
    let base = Duration::from_secs(30);
    // `refused` means the peer is up and declining us: that needs a human, so
    // backing off is right.
    assert_eq!(retry_backoff(base, 0, Some(ConnectClass::Refused)), base);
    assert_eq!(retry_backoff(base, 1, Some(ConnectClass::Refused)), base * 2);
    assert_eq!(retry_backoff(base, 4, Some(ConnectClass::Refused)), base * 60);
    // No class at all (an ack timeout has no failed dial to classify) keeps the
    // historical ladder.
    assert_eq!(retry_backoff(base, 2, None), base * 10);
    assert_eq!(retry_backoff(base, 9, None), base * 60, "the last multiplier is the cap");
}
```

- [x] **Step 2: Run them and watch them fail**

Run: `cargo test -q -p athenaeum-core --lib retry_flat`
Expected: FAIL — `this function takes 2 arguments but 3 were supplied`.

- [x] **Step 3: Implement**

```rust
/// Flat retry interval while the peer looks ABSENT, as a multiple of the base
/// rung — 4 × the default 30 s `ack_timeout` = 2 minutes. Expressed as a
/// multiple, not an absolute, for the same reason `BACKOFF_MULTIPLIERS` is: a
/// test configured with a millisecond timeout then observes the flat path
/// without waiting two minutes.
///
/// Flat by design: escalation exists to spare a loaded peer, and an absent peer
/// is not loaded — climbing to the 30-minute cap only means an overnight outage
/// costs half an hour of idleness after the peer is already back. Affordable only
/// because `serve` no longer re-imports the payload on every attempt (Task 5) and
/// because exactly one package per peer probes (Task 4).
const ABSENT_RETRY_MULTIPLIER: u32 = 4;

/// Whether this failure class says "the peer is not there", as opposed to "the
/// peer refuses us" or "our side is broken".
pub(crate) fn class_is_absent(class: Option<ConnectClass>) -> bool {
    matches!(
        class,
        Some(
            ConnectClass::NoRoute
                | ConnectClass::Timeout
                | ConnectClass::RelayUnreachable
                | ConnectClass::NotStarted
        )
    )
}

pub fn retry_backoff(base: Duration, rung: u32, class: Option<ConnectClass>) -> Duration {
    if class_is_absent(class) {
        return base * ABSENT_RETRY_MULTIPLIER;
    }
    let m = BACKOFF_MULTIPLIERS[(rung as usize).min(BACKOFF_MULTIPLIERS.len() - 1)];
    base * m
}
```

Update both call sites to pass the class: `arm_retry` (`p.last_attempt_class`) and the ack-timeout branch (`self.pending.get(&id).and_then(|p| p.last_attempt_class)` — an ack timeout normally carries `None`, which is the documented, intended path).

Do not climb the rung when the class is absent — in `arm_retry` and the ack-timeout branch, guard the increment:

```rust
            if !class_is_absent(p.last_attempt_class) {
                p.rung = p.rung.saturating_add(1);
            }
```

- [x] **Step 4: Run the tests**

Run: `cargo test -q -p athenaeum-core --lib retry_ && cargo test -q -p athenaeum-core --lib sync::engine`
Expected: PASS. Existing backoff tests that call `retry_backoff(base, rung)` must be updated to pass `None` — that is the historical behaviour.

- [x] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/sync/engine.rs
git commit -m "feat(sync): retry schedule distinguishes an absent peer from a refusing one"
```

---

### Task 4: Per-peer absence, coalescing, and the presence command

**Files:**
- Modify: `crates/athenaeum-core/src/sync/engine.rs`
- Test: `crates/athenaeum-core/src/sync/engine_tests.rs`

**Interfaces:**
- Consumes: `retry_backoff(base, rung, class)`, `class_is_absent` (Task 3).
- Produces: `SyncEngineHandle::peer_present(&self) -> anyhow::Result<()>` — Task 6 calls it.

- [x] **Step 1: Write the failing coalescing test**

In `engine_tests.rs`, in the idiom of the existing `peer_offline_backs_off_and_stays_pending` (line ~1441): a loopback endpoint that is minted but never started makes every announce fail, and that failure classifies as `not_started` — an absent class. `spawn_receiver` (line ~90) is what brings the peer to life afterwards.

```rust
/// While the peer is absent, exactly ONE package probes per interval — otherwise a
/// three-package queue against a shut laptop dials three times every window, and a
/// fifty-package queue fifty times. A presence beacon releases all of them at once.
#[tokio::test]
async fn absent_peer_probes_with_one_package_then_presence_releases_all() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();
    let receiver = net.endpoint();
    let receiver_id = receiver.node_id();

    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let engine = SyncEngine::spawn_with_config(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
        // Short base so the escalating ladder would be obvious if the absent path
        // wrongly used it; the flat `base * ABSENT_RETRY_MULTIPLIER` (here 200 ms)
        // is what must govern instead.
        SyncConfig { ack_timeout: Duration::from_millis(50) },
    );

    let pkg_a = build_package(&tmp.path().join("a"), "uuid-a", "a.fits", "M31", 1024);
    let pkg_b = build_package(&tmp.path().join("b"), "uuid-b", "b.fits", "M31", 1024);
    let pkg_c = build_package(&tmp.path().join("c"), "uuid-c", "c.fits", "M31", 1024);
    let a = engine.enqueue_package(&pkg_a, None, Vec::new()).await.unwrap();
    let b = engine.enqueue_package(&pkg_b, None, Vec::new()).await.unwrap();
    let c = engine.enqueue_package(&pkg_c, None, Vec::new()).await.unwrap();

    // Each package makes its own first attempt (an enqueue is user intent, and the
    // peer state may be stale) — then parks. After that only the head retries, so
    // the attempt counts stop moving together.
    wait_until(|| attempts_of(&store, a) >= 1 && attempts_of(&store, b) >= 1 && attempts_of(&store, c) >= 1, WAIT).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let (pa, pb, pc) = (attempts_of(&store, a), attempts_of(&store, b), attempts_of(&store, c));
    assert!(
        pb == 1 && pc == 1,
        "only the head package probes an absent peer; got a={pa} b={pb} c={pc}"
    );

    // The peer comes to life, then announces itself: every parked package goes.
    spawn_receiver(Arc::clone(&receiver), tmp.path().join("dest"));
    engine.peer_present().await.unwrap();

    for id in [a, b, c] {
        wait_until(|| state_of(&store, id) == Some(OutboundState::Confirmed), WAIT).await;
    }
    engine.shutdown().await;
}

/// The head is re-elected when it leaves `pending`, so a queue whose probing
/// package terminalizes does not go silent forever.
#[tokio::test]
async fn absent_head_is_re_elected_when_it_terminalizes() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();
    let receiver_id = net.endpoint().node_id();

    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let engine = SyncEngine::spawn_with_config(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
        SyncConfig { ack_timeout: Duration::from_millis(50) },
    );

    let head_dir = tmp.path().join("head");
    let pkg_head = build_package(&head_dir, "uuid-h", "h.fits", "M31", 1024);
    let pkg_tail = build_package(&tmp.path().join("tail"), "uuid-t", "t.fits", "M31", 1024);
    let head = engine.enqueue_package(&pkg_head, None, Vec::new()).await.unwrap();
    let tail = engine.enqueue_package(&pkg_tail, None, Vec::new()).await.unwrap();

    wait_until(|| attempts_of(&store, head) >= 1 && attempts_of(&store, tail) >= 1, WAIT).await;

    // Terminalize the head the one way delivery-forever allows: its payload is gone.
    std::fs::remove_dir_all(&head_dir).unwrap();
    wait_until(|| state_of(&store, head) == Some(OutboundState::Failed), WAIT).await;

    // The tail must now be the head — its attempt count climbs again.
    let before = attempts_of(&store, tail);
    wait_until(|| attempts_of(&store, tail) > before, WAIT).await;
    engine.shutdown().await;
}
```

`WAIT` and the helpers (`wait_until`, `attempts_of`, `state_of`, `build_package`, `spawn_receiver`) already exist at the top of `engine_tests.rs`.

- [x] **Step 2: Run it and watch it fail**

Run: `cargo test -q -p athenaeum-core --lib absent_peer_probes_with_one_package`
Expected: FAIL — `no method named peer_present`.

- [x] **Step 3: Implement the peer state**

Add to the engine struct:

```rust
/// The engine's in-memory view of ITS peer's reachability (D1). One engine is
/// one peer, so this needs no keying. Not persisted: after a restart the first
/// attempt re-establishes the truth, and a stale "offline since" would be worse
/// than none.
#[derive(Debug, Default)]
struct PeerReachability {
    /// When the peer first looked absent, stamped only on the transition.
    absent_since: Option<Instant>,
    /// The package currently carrying the live probe deadline while absent.
    head: Option<i64>,
}
```

Rules, in `arm_retry` and the ack-timeout branch:

```rust
        if class_is_absent(class) {
            if self.peer_state.absent_since.is_none() {
                self.peer_state.absent_since = Some(Instant::now());
                tracing::info!(peer = %hex32(&self.peer), "peer looks absent; parking its queue");
            }
            // Elect a head if there is none (or the old one is gone), then park
            // everything else: a parked package waits for a signal, not a clock.
            let head = self.elect_absent_head();
            if Some(id) == head {
                p.deadline = Instant::now() + retry_backoff(self.config.ack_timeout, p.rung, class);
            } else {
                p.deadline = Instant::now() + IDLE_SLEEP;
            }
        }
```

```rust
    /// The package that probes an absent peer: the lowest pending row id, so the
    /// choice is deterministic and stable. Re-elected whenever the previous head
    /// leaves `pending` (confirmed, cancelled, or terminalized locally).
    fn elect_absent_head(&mut self) -> Option<i64> {
        let still_pending = self
            .peer_state
            .head
            .filter(|id| self.pending.contains_key(id));
        let head = still_pending.or_else(|| self.pending.keys().min().copied());
        self.peer_state.head = head;
        head
    }
```

And the release path, called from `Command::PeerPresent` and from every successful contact (`on_ack`, `note_serve_activity`, a successful announce):

```rust
    /// The peer is reachable: forget the absence and un-park everything waiting on
    /// it. Only PARKED packages (`NextAction::Retry`) are touched — a package in
    /// `AwaitAck` with bytes flowing must not be interrupted by a re-announce.
    fn peer_reachable(&mut self) {
        if self.peer_state.absent_since.take().is_some() {
            tracing::info!(peer = %hex32(&self.peer), "peer reachable again; releasing its queue");
        }
        self.peer_state.head = None;
        for p in self.pending.values_mut() {
            if p.next_action == NextAction::Retry {
                p.deadline = Instant::now();
                p.rung = 0;
            }
        }
    }
```

Add the command + handle method:

```rust
    /// A peer-presence signal arrived for this engine's peer (D1).
    PeerPresent,
```

```rust
    /// Tell the worker its peer just announced itself online (D1). Idempotent and
    /// cheap; the host debounces before calling.
    pub async fn peer_present(&self) -> Result<()> {
        self.tx
            .send(Command::PeerPresent)
            .await
            .map_err(|_| anyhow!("sync engine worker is gone"))
    }
```

- [x] **Step 4: Run the tests**

Run: `cargo test -q -p athenaeum-core --lib sync::engine`
Expected: PASS, including the pre-existing engine suite.

- [x] **Step 5: Add the non-disturbance test, run, commit**

```rust
/// Presence must not interrupt a live pull: a package in `AwaitAck` whose peer is
/// actively fetching keeps its deadline and issues no re-announce.
#[tokio::test]
async fn presence_does_not_disturb_an_active_pull() {
    let h = engine_harness().await;
    let id = h.enqueue("pkg").await;
    h.wait_until_awaiting_ack(id).await;
    h.serve_tick(id).await;
    let announces = h.announce_count();

    h.engine.peer_present().await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert_eq!(h.announce_count(), announces, "no re-announce mid-pull");
}
```

```bash
cargo test -q -p athenaeum-core --lib sync::
git add crates/athenaeum-core/src/sync/engine.rs crates/athenaeum-core/src/sync/engine_tests.rs
git commit -m "feat(sync): per-peer absence with one probing head; presence releases the queue"
```

---

### Task 5: `serve` stops re-importing an already-served package

**Files:**
- Modify: `crates/athenaeum-core/src/sharing/iroh/node.rs` (`role_serve`, the `served` bookkeeping)
- Test: `crates/athenaeum-core/src/sharing/iroh/tests.rs`

**Interfaces:**
- Produces: no new public API. `role_serve` keeps its signature; the change is internal short-circuiting.

- [x] **Step 1: Write the failing test**

```rust
/// Re-serving the same package must not re-import it. Audit F8: every retry
/// re-hashed the whole payload before discovering the peer was absent, which is
/// what made frequent retries unaffordable.
#[tokio::test]
async fn re_serving_the_same_package_skips_the_import() {
    let tmp = tempfile::tempdir().unwrap();
    let node = SharedIrohNode::bind(&tmp.path().join("node"), RelayMode::Disabled).await.unwrap();
    let (pkg_dir, announce) = build_one_frame_package(tmp.path());
    let handle = node.role_handle(Role::Out);

    handle.serve(&announce, &pkg_dir, None).await.unwrap();
    let first = node.store().blobs().list().hashes().await.unwrap().len();

    // Mutating the file on disk proves the second serve did not read it: a real
    // re-import would hash the NEW bytes and land a different blob.
    std::fs::write(pkg_dir.join("frame.fits"), b"different bytes entirely").unwrap();
    handle.serve(&announce, &pkg_dir, None).await.unwrap();
    let second = node.store().blobs().list().hashes().await.unwrap().len();

    assert_eq!(first, second, "the second serve imported nothing");
    node.shutdown().await;
}
```

- [x] **Step 2: Run it and watch it fail**

Run: `cargo test -q -p athenaeum-core --lib re_serving_the_same_package`
Expected: FAIL — the blob count grows, because the mutated file is re-imported.

- [x] **Step 3: Implement the short-circuit**

Record the want fingerprint alongside the hash, and skip on an exact match:

```rust
/// Fingerprint of a serve's want-subset: `None` (full package) or the sorted
/// rel_paths joined. Two serves of the same package with DIFFERENT subsets must
/// not share a collection, so the short-circuit keys on this, not on the tag alone.
fn want_fingerprint(want: Option<&HashSet<String>>) -> Option<String> {
    want.map(|w| {
        let mut v: Vec<&str> = w.iter().map(String::as_str).collect();
        v.sort_unstable();
        v.join("\n")
    })
}
```

In `role_serve`, before importing:

```rust
        let fingerprint = want_fingerprint(want);
        if let Some(hash) = self.served_hash_for(&tag, &fingerprint) {
            tracing::debug!(package_id = %pkg.package_id.0, %hash, "serve reuses the already-imported collection");
            return Ok(());
        }
```

`served` becomes `HashMap<String, (Hash, Option<String>)>` (hash + fingerprint); `served_hash_for` returns the hash only when the fingerprint matches. `release` already clears the entry, so a rebuilt payload (Perseus resend) re-imports correctly, and after a restart the map is empty so the first serve imports again — correct, since the blob store's tags may have been swept.

Update the two readers of `served` (`resolve_served_root_in`, `resolve_served_file_in`) for the new tuple shape.

- [x] **Step 4: Run the tests**

Run: `cargo test -q -p athenaeum-core --lib sharing::iroh`
Expected: PASS, whole module.

- [x] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/sharing/iroh/node.rs crates/athenaeum-core/src/sharing/iroh/tests.rs
git commit -m "perf(sync): serve reuses an already-imported package instead of re-hashing it"
```

---

### Task 6: Host wiring — gates, ensure-then-kick, and the two beacon edges

**Files:**
- Modify: `crates/athenaeum-core/src/sync/sender.rs` (`kick_peer`)
- Modify: `crates/athenaeum-core/src/api/sync.rs` (presence hook install + gates; beacon fan-out; call it from the two edges)
- Test: `crates/athenaeum-core/src/api/sync.rs` `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `SharedIrohNode::set_presence_hook` (Task 2), `SyncEngineHandle::peer_present` (Task 4).
- Produces: `SyncSenderRuntime::kick_peer(&self, peer: &NodeId)`; `api::sync::broadcast_presence(ctx, node)`.

- [x] **Step 1: Write the failing gate tests**

```rust
/// A presence beacon must never make us allocate an engine for a stranger: both
/// gates are LOCAL facts, checked before anything is built.
#[tokio::test]
async fn presence_from_an_unauthorized_peer_builds_no_engine() {
    let ctx = test_ctx_signed_in(&["aa".repeat(32)]).await;
    let sender = Arc::new(SyncSenderRuntime::new());
    let stranger: NodeId = [7u8; 32];

    handle_peer_presence(&ctx, &sender, &sender, &SyncRuntime::default(), &noop_emitter(), stranger).await;

    assert!(sender.started_peers().await.is_empty(), "no engine for an unauthorized peer");
}

/// An authorized peer with nothing pending is also a no-op — presence is a
/// resume signal, not an engine-construction trigger.
#[tokio::test]
async fn presence_with_no_pending_rows_builds_no_engine() {
    let ctx = test_ctx_signed_in(&[hex32(&PEER)]).await;
    let sender = Arc::new(SyncSenderRuntime::new());

    handle_peer_presence(&ctx, &sender, &sender, &SyncRuntime::default(), &noop_emitter(), PEER).await;

    assert!(sender.started_peers().await.is_empty(), "nothing to resume, nothing to build");
}
```

- [x] **Step 2: Run them and watch them fail**

Run: `cargo test -q -p athenaeum-core --lib presence_from_an_unauthorized_peer`
Expected: FAIL — `cannot find function handle_peer_presence`.

- [x] **Step 3: Implement `kick_peer`**

In `crates/athenaeum-core/src/sync/sender.rs`, beside `kick_all`:

```rust
    /// Tell ONE peer's engine that its peer is present (D1), if that engine is
    /// started. Fire-and-forget and log-and-continue, like [`kick_all`]; the
    /// caller (`api::sync::handle_peer_presence`) is what guarantees an engine
    /// exists when there is work for it.
    pub async fn kick_peer(&self, peer: &NodeId) {
        let engine = self.inner.lock().await.get(peer).map(|s| Arc::clone(&s.engine));
        if let Some(engine) = engine {
            if let Err(e) = engine.peer_present().await {
                tracing::warn!(error = %e, peer = %hex32(peer), "peer-present kick failed");
            }
        }
    }
```

- [x] **Step 4: Implement the gated handler + beacon**

In `crates/athenaeum-core/src/api/sync.rs`:

```rust
/// Debounce window for inbound presence, per peer. A peer whose relay flaps would
/// otherwise turn into a burst of attempts.
const PRESENCE_DEBOUNCE: Duration = Duration::from_secs(10);

/// How many beacons are in flight at once during a fan-out. A startup beacon must
/// never make the app wait on dead peers.
const PRESENCE_FANOUT_CONCURRENCY: usize = 4;

/// Handle one inbound presence beacon (D1 §3.2): ensure-then-kick, behind two
/// gates evaluated from LOCAL state only — the peer must be an authorized account
/// device, and we must hold at least one non-terminal outbound row for it.
/// Without the first, a stranger could make us allocate engines; without the
/// second, a beacon from an idle device would build engines for nothing.
pub async fn handle_peer_presence(
    ctx: &Arc<ServiceContext>,
    sender: &Arc<SyncSenderRuntime>,
    collab_sender: &Arc<SyncSenderRuntime>,
    sync: &SyncRuntime,
    emitter: &Arc<dyn ProgressEmitter>,
    from: NodeId,
) {
    let hex = node_id_hex(&from);
    if !cached_authorized_peer_hexes(ctx).contains(&hex) {
        tracing::warn!(peer = %hex, "presence from an unauthorized peer; ignored");
        return;
    }
    if !has_non_terminal_rows_for(ctx, &from) {
        tracing::debug!(peer = %hex, "presence from an authorized peer with nothing pending");
        return;
    }
    // Ensure-then-kick: a beacon that beats `resurrect_pending_senders` (or lands
    // on a device whose cached allow-list was empty when it ran) would otherwise
    // find no engine and be silently wasted.
    if sender.current_for(&from).await.is_none() {
        // Same seven arguments `resurrect_pending_senders` passes: no reported
        // endpoint address (the beacon just proved the peer is dialable on the path
        // it arrived by) and no emitter override.
        if let Err(e) = ensure_sender_engine(
            ctx,
            sender,
            Arc::clone(collab_sender),
            sync,
            from,
            None,
            Some(Arc::clone(emitter)),
        )
        .await
        {
            tracing::warn!(peer = %hex, error = %format!("{e:#}"), "presence: engine build failed");
            return;
        }
    }
    tracing::info!(peer = %hex, "peer presence; resuming its queue");
    sender.kick_peer(&from).await;
    collab_sender.kick_peer(&from).await;
}

/// Announce our presence to every authorized ACCOUNT device (D1 §3.1). Account
/// devices only: collab holders belong to other accounts, and telling them when we
/// come online leaks a presence signal nobody asked for.
pub async fn broadcast_presence(ctx: &Arc<ServiceContext>, node: &Arc<SharedIrohNode>) {
    let peers = cached_authorized_peer_hexes(ctx);
    let mut tasks = tokio::task::JoinSet::new();
    let mut inflight = 0usize;
    for hex in peers {
        let Some(peer) = node_id_from_hex(&hex) else { continue };
        if peer == node.node_id() { continue; } // never beacon ourselves
        let node = Arc::clone(node);
        tasks.spawn(async move {
            if let Err(e) = node.send_presence(peer).await {
                tracing::debug!(peer = %hex, error = %format!("{e:#}"), "presence beacon undelivered");
            }
        });
        inflight += 1;
        if inflight >= PRESENCE_FANOUT_CONCURRENCY {
            let _ = tasks.join_next().await;
            inflight -= 1;
        }
    }
    while tasks.join_next().await.is_some() {}
    tracing::info!("presence beacon fan-out complete");
}
```

Install the hook where the wake hook is installed (`install_node_wake_hook`), applying the debounce there, and call `broadcast_presence` from the two edges: right after the receiver is spawned in the autostart path, and inside the wake hook itself (relay reconnect).

- [x] **Step 5: Run the tests**

Run: `cargo test -q -p athenaeum-core --lib api::sync`
Expected: PASS.

- [x] **Step 6: Full gates + commit**

```bash
cargo build -q --workspace --all-targets && cargo build -q -p perseus --no-default-features
cargo test -q -p athenaeum-core --lib
git add crates/athenaeum-core/src/sync/sender.rs crates/athenaeum-core/src/api/sync.rs
git commit -m "feat(sync): presence beacon on the two reachability edges; gated ensure-then-kick"
```

---

### Task 7: Honest row text for a parked transfer

**Files:**
- Modify: `crates/athenaeum-core/src/sync/status.rs` (the waiting-state mapping)
- Modify: `src/components/transfers/TransferRow.tsx` (render the peer-absent case)
- Test: `crates/athenaeum-core/src/sync/status.rs` `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: the `class` prefix already carried in `OutboundRow::last_error`.
- Produces: no new type; `OutboundSummary.displayState` gains the value `"waiting_peer"`.

- [ ] **Step 1: Write the failing test**

```rust
/// A package parked because its peer is absent must not render a countdown to an
/// arbitrary retry — it is waiting for a signal, not a clock.
#[test]
fn a_peer_absent_row_reads_as_waiting_for_the_peer() {
    let row = outbound_row_with(OutboundState::Announced, Some("no_route: no known addresses"));
    let s = outbound_summary(row, now(), &names(), &counts());
    assert_eq!(s.display_state, "waiting_peer");
    assert_eq!(s.stalled_until, None, "no countdown for a signal-driven wait");
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -q -p athenaeum-core --lib a_peer_absent_row_reads`
Expected: FAIL — `display_state` is `"waiting"` with a `stalled_until`.

- [ ] **Step 3: Implement the mapping**

In `status.rs`, where `display_state`/`stalled_until` are derived, add the branch before the existing `waiting` case:

```rust
/// The machine-readable class prefixes a failed dial writes into `last_error`
/// (`sync::diagnostics::ConnectClass::tag`) that mean "the peer is not there".
/// Kept in sync with `engine::class_is_absent` by
/// `absent_prefixes_match_the_engine_classes`.
const PEER_ABSENT_PREFIXES: [&str; 4] =
    ["no_route:", "timeout:", "relay_unreachable:", "not_started:"];

fn peer_looks_absent(last_error: Option<&str>) -> bool {
    last_error.is_some_and(|e| PEER_ABSENT_PREFIXES.iter().any(|p| e.starts_with(p)))
}
```

```rust
    // A package parked on an absent peer waits for a SIGNAL, not a clock: showing
    // a countdown to the next flat probe invites the user to believe the number
    // means something. The row says who it is waiting for instead.
    if !terminal && peer_looks_absent(row.last_error.as_deref()) {
        return ("waiting_peer".to_string(), None);
    }
```

Add the drift guard beside it:

```rust
#[test]
fn absent_prefixes_match_the_engine_classes() {
    for class in [
        ConnectClass::NoRoute,
        ConnectClass::Timeout,
        ConnectClass::RelayUnreachable,
        ConnectClass::NotStarted,
    ] {
        let rendered = format!("{}: something", class.tag());
        assert!(
            peer_looks_absent(Some(&rendered)),
            "{} is an absent class in the engine but not in the status mapper",
            class.tag()
        );
    }
    assert!(!peer_looks_absent(Some("refused: not on the peer's allow-list")));
}
```

- [ ] **Step 4: Render it**

In `src/components/transfers/TransferRow.tsx`, map `waiting_peer` to `waiting for {deviceName} — unreachable` (device name already available on the row; fall back to the short peer id). Use design tokens (`text-content-muted`), never raw colors.

- [ ] **Step 5: Run the gates**

```bash
cargo test -q -p athenaeum-core --lib sync::status
npx tsc --noEmit
```

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-core/src/sync/status.rs src/components/transfers/TransferRow.tsx
git commit -m "feat(transfers): a peer-absent transfer says so instead of counting down"
```

---

## After the last task

Run the whole gate set once more, then update the spec's status line and the audit's D1 row to point at the merge commit:

```bash
cargo build -q --workspace --all-targets
cargo build -q -p perseus --no-default-features
cargo test -q -p athenaeum-core --lib
npx tsc --noEmit
```

**Owner smoke (cannot be automated here):** two machines, real cross-NAT. Close the receiver mid-transfer, confirm the sender's row reads "waiting … unreachable" with no countdown; restart the receiver and confirm delivery resumes within seconds of its startup (not after a ladder wait); then repeat with the SENDER closed to confirm `resurrect_pending_senders` still carries that direction. Watch for `peer presence; resuming its queue` on the sender and `presence beacon fan-out complete` on the receiver.
